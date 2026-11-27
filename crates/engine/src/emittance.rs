//! Emission a reference's `XEMI` light or region lends the `External_Emittance` surfaces of its
//! model (#242).
//!
//! A NIF shader with `SLSF1_External_Emittance` takes its emission colour from outside the model:
//! from the light or region its reference names in `XEMI`. Building windows name regions whose
//! weather is black by day and amber at night, which is how Skyrim keeps windows dark in daylight.
//! The converter resolves every source's colour at each time of day into the `emittance` table
//! (`docs/specs/converters/db-schema.md`). `mudcrab` multiplies the model's emission by the day
//! colour. `skyrim` mixes the day parts by the game hour and takes the bytes without a decode.
//!
//! A surface whose reference names no source, or one the converter could not resolve, keeps the
//! model's own emission.
use crate::{
    nif_depth::NifDepthMaterial, nif_material::ExternalEmittance, retail_lighting::LightUnits,
};
use bevy::{app::SceneSpawnerSystems, prelude::*};
use std::collections::HashMap;

/// Register after `NifMaterialPlugin`, which marks the surfaces this applies to.
pub struct EmittancePlugin;

impl Plugin for EmittancePlugin {
    fn build(&self, app: &mut App) {
        register_emittance::<StandardMaterial>(app);
        register_emittance::<NifDepthMaterial>(app);
        app.add_systems(Update, drive_emittance_by_hour);
    }
}

/// The four day-part colours of a reference's `XEMI` source (sunrise, day, sunset, night), on a
/// reference under the `skyrim` profile only.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct EmittanceDayParts(pub [[u8; 3]; 4]);

impl EmittanceDayParts {
    /// The source's colour at these day-part weights: the weighted sum of the bytes, rounded to
    /// a byte. Rounding keeps a running clock from making a new material every frame.
    pub fn mixed(&self, weights: [f32; 4]) -> [u8; 3] {
        std::array::from_fn(|channel| {
            let sum: f32 = self
                .0
                .iter()
                .zip(weights)
                .map(|(part, weight)| weight * f32::from(part[channel]))
                .sum();
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a weighted mean of bytes, clamped to 0..=255"
            )]
            let byte = sum.round().clamp(0.0, 255.0) as u8;
            byte
        })
    }
}

/// Follows the game clock under the `skyrim` profile: a source's colour is the mix of its day
/// parts by the sky's weights (SPEC-1 section 2.6, `docs/specs/engine/near-lighting.md`). It
/// writes the colour the reference's emitting surfaces take, and the colour of its light, the
/// record bytes / 255 times the source's. Without a climate timing the day colour stays.
fn drive_emittance_by_hour(
    clock: Option<Res<crate::sun::GameClock>>,
    config: Option<Res<crate::config::EngineConfig>>,
    catalog: Option<Res<crate::world::environment::EnvironmentCatalog>>,
    mut references: Query<(
        &EmittanceDayParts,
        &mut ReferenceEmittance,
        Option<&crate::lights::LightEvaluation>,
        Option<&Children>,
    )>,
    mut lights: Query<&mut PointLight, With<crate::lights::SkyrimLight>>,
) {
    let (Some(clock), Some(config), Some(catalog)) = (clock, config, catalog) else {
        return;
    };
    let Some(timing) = catalog
        .worldspace_climate(config.worldspace_id)
        .and_then(|climate| catalog.sun_timing(climate))
    else {
        return;
    };
    let weights = crate::weather_sky::slot_weights(clock.hour, timing);
    for (parts, mut emittance, evaluation, children) in &mut references {
        let source = parts.mixed(weights);
        emittance.set_if_neq(ReferenceEmittance::of(LightUnits::Retail, source));
        let (Some(evaluation), Some(children)) = (evaluation, children) else {
            continue;
        };
        let Some(wanted) = evaluation.point_light_in(LightUnits::Retail, Some(source)) else {
            continue;
        };
        for &child in children {
            if let Ok(mut light) = lights.get_mut(child)
                && light.color != wanted.color
            {
                light.color = wanted.color;
            }
        }
    }
}

fn register_emittance<M: EmittingMaterial>(app: &mut App) {
    app.init_resource::<EmittanceMaterials<M>>()
        .add_systems(
            SpawnScene,
            apply_reference_emittance::<M>.after(SceneSpawnerSystems::WorldInstanceSpawn),
        )
        .add_systems(Update, forget_freed_materials::<M>);
}

/// The colour a reference's `XEMI` source lends its model's `External_Emittance` surfaces, as
/// the profile's shaders read it.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct ReferenceEmittance(pub LinearRgba);

impl ReferenceEmittance {
    /// The source's record bytes in the scene's units. `mudcrab` decodes them as sRGB. `skyrim`
    /// takes `byte / 255` with no decode (SPEC-1 section 2.1,
    /// `docs/specs/engine/near-lighting.md`), for lit and effect surfaces alike. A decode there
    /// turns the Whiterun windows' dusk orange into red.
    pub fn of(units: LightUnits, rgb: [u8; 3]) -> Self {
        let authored = Color::srgb_u8(rgb[0], rgb[1], rgb[2]);
        Self(match units {
            LightUnits::Mudcrab => authored.to_linear(),
            LightUnits::Retail => crate::retail_lighting::raw_colour(authored).to_linear(),
        })
    }
}

/// The material a surface drew with before its reference's emittance was applied, so a second
/// application starts again from the model's own emission instead of compounding.
#[derive(Component, Clone)]
struct EmittanceSourceMaterial<M: Asset>(Handle<M>);

/// One material per model material and emittance colour, shared by every reference that lends the
/// same colour, so emitting references keep batching together. The cache holds ids, not handles:
/// a derived material is freed with the last surface that draws with it, as a cell unloads, and
/// [`forget_freed_materials`] then drops its entry.
#[derive(Resource)]
struct EmittanceMaterials<M: Asset = StandardMaterial>(HashMap<(AssetId<M>, [u32; 3]), AssetId<M>>);

impl<M: Asset> Default for EmittanceMaterials<M> {
    fn default() -> Self {
        Self(HashMap::new())
    }
}

trait EmittingMaterial: bevy::pbr::Material + Clone {
    fn base_mut(&mut self) -> &mut StandardMaterial;

    fn uses_additive_source_colour(&self) -> bool {
        false
    }
}

impl EmittingMaterial for StandardMaterial {
    fn base_mut(&mut self) -> &mut StandardMaterial {
        self
    }
}

impl EmittingMaterial for NifDepthMaterial {
    fn base_mut(&mut self) -> &mut StandardMaterial {
        &mut self.base
    }

    fn uses_additive_source_colour(&self) -> bool {
        self.extension.state.uses_effect_emittance
    }
}

/// `material` with its emission multiplied by `colour`. The published emission is the NIF's tint
/// times its multiplier (`docs/specs/engine/color-pipeline.md`), which the glTF cannot separate, so
/// the external colour multiplies the tint rather than replacing it; the window shaders that use
/// external emittance have a white tint, where the two agree.
fn lit_by<M: EmittingMaterial>(material: &M, colour: LinearRgba) -> M {
    let mut material = material.clone();
    let additive = material.uses_additive_source_colour();
    let base = material.base_mut();
    let mut tint = if additive {
        base.base_color.to_linear()
    } else {
        base.emissive
    };
    match base.retail_surface.filter(|_| !additive) {
        // Under `skyrim` the source replaces the emissive colour: the emission is the source's
        // bytes / 255 times the model's multiple, and the file colour is not multiplied in
        // (SPEC-1 section 2.7, docs/specs/engine/near-lighting.md).
        Some(surface) => {
            tint.red = colour.red * surface.emissive_multiple;
            tint.green = colour.green * surface.emissive_multiple;
            tint.blue = colour.blue * surface.emissive_multiple;
        }
        None => {
            tint.red *= colour.red;
            tint.green *= colour.green;
            tint.blue *= colour.blue;
        }
    }
    if additive {
        base.base_color = Color::LinearRgba(tint);
    } else {
        base.emissive = tint;
    }
    material
}

/// A model's `External_Emittance` surfaces: the material each draws with and, once a reference's
/// colour has been applied, the model's own material it started from.
type ExternalEmittanceSurfaces<'world, 'state, M> = Query<
    'world,
    'state,
    (
        Entity,
        &'static mut MeshMaterial3d<M>,
        Option<&'static EmittanceSourceMaterial<M>>,
    ),
    With<ExternalEmittance>,
>;

/// Marks a reference with a surface whose model material had not loaded when its scene was
/// instantiated, so [`apply_reference_emittance`] tries again once a material arrives.
#[derive(Component)]
struct AwaitingEmittanceMaterial<M: Asset = StandardMaterial>(std::marker::PhantomData<M>);

/// Emitting references to visit: a newly instantiated model, or one still waiting for a material.
type EmittingReferences<'world, 'state, M> = Query<
    'world,
    'state,
    (
        Entity,
        Ref<'static, ReferenceEmittance>,
        Ref<'static, Children>,
        Has<AwaitingEmittanceMaterial<M>>,
    ),
    Or<(
        Changed<Children>,
        Changed<ReferenceEmittance>,
        With<AwaitingEmittanceMaterial<M>>,
    )>,
>;

/// Gives every `External_Emittance` surface of a newly instantiated model its reference's colour.
///
/// A reference gains a child the moment Bevy writes its converted scene below it, so
/// `Changed<Children>` on an emitting reference is the frame its surfaces exist. A model can be
/// instantiated before its materials finish loading; a surface whose material is not there yet is
/// left as it is and its reference waits, to be visited again when a material is added.
fn apply_reference_emittance<M: EmittingMaterial>(
    mut commands: Commands,
    references: EmittingReferences<M>,
    descendants: Query<&Children>,
    mut surfaces: ExternalEmittanceSurfaces<M>,
    mut materials: ResMut<Assets<M>>,
    mut cache: ResMut<EmittanceMaterials<M>>,
    mut material_events: MessageReader<AssetEvent<M>>,
) {
    let material_arrived = material_events.read().any(|event| {
        matches!(
            event,
            AssetEvent::Added { .. } | AssetEvent::LoadedWithDependencies { .. }
        )
    });
    for (reference, emittance, children, awaiting) in &references {
        if !children.is_changed() && !emittance.is_changed() && !material_arrived {
            continue;
        }
        let mut missing = false;
        let colour = emittance.0;
        let colour_key = [colour.red, colour.green, colour.blue].map(f32::to_bits);
        for descendant in descendants.iter_descendants(reference) {
            let Ok((surface, mut material, source)) = surfaces.get_mut(descendant) else {
                continue;
            };
            let source = match source {
                Some(source) => source.0.clone(),
                None => {
                    commands
                        .entity(surface)
                        .insert(EmittanceSourceMaterial(material.0.clone()));
                    material.0.clone()
                }
            };
            let key = (source.id(), colour_key);
            let cached = cache
                .0
                .get(&key)
                .and_then(|&lit| materials.get_strong_handle(lit));
            let lit = match cached {
                Some(lit) => lit,
                None => {
                    let Some(lit) = materials.get(&source).map(|base| lit_by(base, colour)) else {
                        missing = true;
                        continue;
                    };
                    let lit = materials.add(lit);
                    cache.0.insert(key, lit.id());
                    lit
                }
            };
            if material.0 != lit {
                material.0 = lit;
            }
        }
        match (missing, awaiting) {
            (true, false) => {
                commands
                    .entity(reference)
                    .insert(AwaitingEmittanceMaterial::<M>(std::marker::PhantomData));
            }
            (false, true) => {
                commands
                    .entity(reference)
                    .remove::<AwaitingEmittanceMaterial<M>>();
            }
            _ => {}
        }
    }
}

/// Drops the cache entries of materials Bevy has freed: a derived material whose last surface
/// unloaded, or a model material whose model did.
fn forget_freed_materials<M: Asset>(
    mut events: MessageReader<AssetEvent<M>>,
    mut cache: ResMut<EmittanceMaterials<M>>,
) {
    for event in events.read() {
        if let AssetEvent::Removed { id } = event {
            cache.0.retain(|(source, _), lit| source != id && lit != id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Asset tracking runs, so a material whose last handle drops is freed as in the engine.
    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<StandardMaterial>()
            .init_asset::<NifDepthMaterial>()
            .init_resource::<EmittanceMaterials<NifDepthMaterial>>()
            .init_resource::<EmittanceMaterials>()
            .add_systems(
                Update,
                (
                    apply_reference_emittance::<StandardMaterial>,
                    forget_freed_materials::<StandardMaterial>,
                    apply_reference_emittance::<NifDepthMaterial>,
                )
                    .chain(),
            );
        app
    }

    fn glowing(app: &mut App, emissive: LinearRgba) -> Handle<StandardMaterial> {
        app.world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial {
                emissive,
                ..default()
            })
    }

    fn emissive(app: &App, entity: Entity) -> LinearRgba {
        let handle = &app
            .world()
            .get::<MeshMaterial3d<StandardMaterial>>(entity)
            .unwrap()
            .0;
        app.world()
            .resource::<Assets<StandardMaterial>>()
            .get(handle)
            .unwrap()
            .emissive
    }

    /// A reference with a model below it: one external-emittance surface nested under a scene root,
    /// and one ordinary surface sharing its material.
    fn reference(
        app: &mut App,
        emittance: Option<[u8; 3]>,
        material: &Handle<StandardMaterial>,
    ) -> (Entity, Entity, Entity) {
        let world = app.world_mut();
        let window = world
            .spawn((MeshMaterial3d(material.clone()), ExternalEmittance))
            .id();
        let wall = world.spawn(MeshMaterial3d(material.clone())).id();
        let root = world.spawn_empty().add_children(&[window, wall]).id();
        let mut reference = world.spawn_empty();
        if let Some(rgb) = emittance {
            reference.insert(ReferenceEmittance::of(LightUnits::Mudcrab, rgb));
        }
        let reference = reference.add_child(root).id();
        (reference, window, wall)
    }

    #[test]
    fn a_black_day_colour_darkens_only_the_external_emittance_surface() {
        let mut app = app();
        let glow = LinearRgba::new(3.0, 3.0, 3.0, 1.0);
        let material = glowing(&mut app, glow);
        let (_, window, wall) = reference(&mut app, Some([0, 0, 0]), &material);
        app.update();
        assert_eq!(emissive(&app, window), LinearRgba::new(0.0, 0.0, 0.0, 1.0));
        assert_eq!(
            emissive(&app, wall),
            glow,
            "the wall keeps the model's emission"
        );
        assert_eq!(
            app.world()
                .resource::<Assets<StandardMaterial>>()
                .get(&material)
                .unwrap()
                .emissive,
            glow,
            "the model's material is not changed in place"
        );
    }

    #[test]
    fn the_day_colour_applies_to_a_depth_material_without_changing_its_depth_state() {
        use crate::nif_depth::{NifDepthExtension, NifDepthState};

        let mut app = app();
        let state = NifDepthState {
            depth_test: true,
            depth_write: false,
            decal: true,
            alpha_mask: false,
            cast_shadows: false,
            blend_factors: None,
            retail_blend_mode: None,
            uses_effect_emittance: false,
            first_stage: false,
        };
        let glow = LinearRgba::new(2.0, 2.0, 2.0, 1.0);
        let material = app
            .world_mut()
            .resource_mut::<Assets<NifDepthMaterial>>()
            .add(NifDepthMaterial {
                base: StandardMaterial {
                    emissive: glow,
                    alpha_mode: AlphaMode::Blend,
                    ..default()
                },
                extension: NifDepthExtension { state },
            });
        let surface = app
            .world_mut()
            .spawn((MeshMaterial3d(material.clone()), ExternalEmittance))
            .id();
        app.world_mut()
            .spawn(ReferenceEmittance::of(LightUnits::Mudcrab, [0, 0, 0]))
            .add_child(surface);
        app.update();

        let world = app.world();
        let derived = &world
            .get::<MeshMaterial3d<NifDepthMaterial>>(surface)
            .unwrap()
            .0;
        let assets = world.resource::<Assets<NifDepthMaterial>>();
        assert_eq!(assets.get(&material).unwrap().base.emissive, glow);
        let drawn = assets.get(derived).unwrap();
        assert_eq!(drawn.base.emissive, LinearRgba::new(0.0, 0.0, 0.0, 1.0));
        assert_eq!(drawn.base.alpha_mode, AlphaMode::Blend);
        assert_eq!(drawn.extension.state, state);
    }

    #[test]
    fn additive_glow_scales_source_rgb_and_preserves_alpha() {
        use crate::nif_depth::{DECAL, DEPTH_TEST, depth_material};

        let mut state =
            crate::nif_depth::NifDepthState::from_flags(DEPTH_TEST | DECAL, 0, AlphaMode::Blend)
                .unwrap();
        state.uses_effect_emittance = true;
        let material = depth_material(
            StandardMaterial {
                base_color: Color::linear_rgba(2.0, 2.0, 2.0, 0.5),
                unlit: true,
                alpha_mode: AlphaMode::Blend,
                ..default()
            },
            state,
        );

        let daylight = lit_by(&material, LinearRgba::rgb(0.0, 0.0, 0.0));
        let night = lit_by(&material, LinearRgba::rgb(1.0, 0.5, 0.25));

        assert_eq!(
            daylight.base.base_color,
            Color::linear_rgba(0.0, 0.0, 0.0, 0.5)
        );
        assert_eq!(
            night.base.base_color,
            Color::linear_rgba(2.0, 1.0, 0.5, 0.5)
        );
        assert_eq!(night.extension.state, state);
    }

    #[test]
    fn the_colour_multiplies_the_published_emission_once() {
        let mut app = app();
        let material = glowing(&mut app, LinearRgba::new(3.0, 2.0, 1.0, 1.0));
        let (reference, window, _) = reference(&mut app, Some([255, 128, 0]), &material);
        app.update();
        let colour = Color::srgb_u8(255, 128, 0).to_linear();
        let expected = LinearRgba::new(3.0, 2.0 * colour.green, 0.0, 1.0);
        assert_eq!(emissive(&app, window), expected);

        // A later change below the reference applies the colour again, from the model's material.
        let extra = app.world_mut().spawn_empty().id();
        app.world_mut().entity_mut(reference).add_child(extra);
        app.update();
        assert_eq!(emissive(&app, window), expected, "no compounding");
    }

    #[test]
    fn references_lending_the_same_colour_share_one_material() {
        let mut app = app();
        let material = glowing(&mut app, LinearRgba::new(3.0, 3.0, 3.0, 1.0));
        let (_, first, _) = reference(&mut app, Some([242, 176, 55]), &material);
        let (_, second, _) = reference(&mut app, Some([242, 176, 55]), &material);
        let (_, other, _) = reference(&mut app, Some([0, 0, 0]), &material);
        app.update();
        let handle = |entity: Entity| {
            app.world()
                .get::<MeshMaterial3d<StandardMaterial>>(entity)
                .unwrap()
                .0
                .clone()
        };
        assert_eq!(handle(first), handle(second));
        assert_ne!(handle(first), handle(other));
        assert_ne!(handle(first), material);
    }

    /// A model instantiated before its material loaded keeps its emission until the material
    /// arrives, then takes its reference's colour.
    #[test]
    fn a_surface_whose_material_loads_late_gets_the_colour_when_it_arrives() {
        let mut app = app();
        let late = app
            .world()
            .resource::<Assets<StandardMaterial>>()
            .reserve_handle();
        let (placed, window, _) = reference(&mut app, Some([0, 0, 0]), &late);
        app.update();
        assert_eq!(
            app.world()
                .get::<MeshMaterial3d<StandardMaterial>>(window)
                .unwrap()
                .0,
            late,
            "nothing to multiply yet"
        );
        assert!(
            app.world()
                .get::<AwaitingEmittanceMaterial>(placed)
                .is_some()
        );

        app.world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .insert(
                &late,
                StandardMaterial {
                    emissive: LinearRgba::new(3.0, 3.0, 3.0, 1.0),
                    ..default()
                },
            )
            .unwrap();
        for _ in 0..3 {
            app.update();
        }
        assert_eq!(emissive(&app, window), LinearRgba::new(0.0, 0.0, 0.0, 1.0));
        assert!(
            app.world()
                .get::<AwaitingEmittanceMaterial>(placed)
                .is_none()
        );
    }

    /// A cell unloading despawns its references. The material derived for their surfaces is freed
    /// with them instead of staying in the cache, and a reference loaded later gets a new one.
    #[test]
    fn a_derived_material_is_freed_with_its_last_surface() {
        let mut app = app();
        let material = glowing(&mut app, LinearRgba::new(3.0, 3.0, 3.0, 1.0));
        let colour = Color::srgb_u8(242, 176, 55).to_linear();
        let lit = LinearRgba::new(3.0 * colour.red, 3.0 * colour.green, 3.0 * colour.blue, 1.0);
        let (placed, window, _) = reference(&mut app, Some([242, 176, 55]), &material);
        app.update();
        let derived = app
            .world()
            .get::<MeshMaterial3d<StandardMaterial>>(window)
            .unwrap()
            .0
            .id();

        app.world_mut().entity_mut(placed).despawn();
        for _ in 0..3 {
            app.update();
        }
        assert!(
            !app.world()
                .resource::<Assets<StandardMaterial>>()
                .contains(derived),
            "the derived material is freed"
        );
        assert!(
            app.world().resource::<EmittanceMaterials>().0.is_empty(),
            "and the cache forgets it"
        );

        let (_, window, _) = reference(&mut app, Some([242, 176, 55]), &material);
        app.update();
        assert_eq!(emissive(&app, window), lit);
    }

    #[test]
    fn a_reference_without_a_source_keeps_the_model_emission() {
        let mut app = app();
        let glow = LinearRgba::new(3.0, 3.0, 3.0, 1.0);
        let material = glowing(&mut app, glow);
        let (_, window, _) = reference(&mut app, None, &material);
        app.update();
        assert_eq!(emissive(&app, window), glow);
        assert_eq!(
            app.world()
                .get::<MeshMaterial3d<StandardMaterial>>(window)
                .unwrap()
                .0,
            material
        );
    }

    /// The fuzz of the decode fault behind the red Whiterun windows: every byte value, on every
    /// path a source colour takes into a surface or a light, in both profiles' colour spaces.
    #[test]
    fn every_path_takes_the_source_bytes_in_the_profile_colour_space() {
        use crate::nif_depth::{DECAL, DEPTH_TEST, NifDepthState, depth_material};
        use bevy::pbr::RetailSurface;

        let multiple = 2.0;
        let published = LinearRgba::rgb(0.5, 1.0, 2.0);
        let lit_surface = StandardMaterial {
            retail_surface: Some(RetailSurface {
                emissive_multiple: multiple,
                ..RetailSurface::MATTE
            }),
            ..default()
        };
        let emitting = StandardMaterial {
            emissive: published,
            ..default()
        };
        let mut state = NifDepthState::from_flags(DEPTH_TEST | DECAL, 0, AlphaMode::Blend).unwrap();
        state.uses_effect_emittance = true;
        let glow = depth_material(
            StandardMaterial {
                base_color: published.into(),
                unlit: true,
                alpha_mode: AlphaMode::Blend,
                ..default()
            },
            state,
        );
        let white = crate::lights::evaluate_light(&white_light(), None);
        for byte in 0..=u8::MAX {
            let rgb = [byte, byte.wrapping_add(85), byte.wrapping_add(170)];
            for units in [LightUnits::Mudcrab, LightUnits::Retail] {
                let expected = rgb.map(|part| {
                    let authored = f32::from(part) / 255.0;
                    match units {
                        LightUnits::Retail => authored,
                        LightUnits::Mudcrab => Color::srgb(authored, 0.0, 0.0).to_linear().red,
                    }
                });
                let colour = ReferenceEmittance::of(units, rgb).0;
                let close = |found: LinearRgba, scale: [f32; 3]| {
                    let found = [found.red, found.green, found.blue];
                    for channel in 0..3 {
                        let wanted = expected[channel] * scale[channel];
                        assert!(
                            (found[channel] - wanted).abs() < 1.0e-5,
                            "{units:?} {rgb:?}: {found:?} channel {channel} wants {wanted}"
                        );
                    }
                };
                let base = [published.red, published.green, published.blue];
                close(lit_by(&emitting, colour).emissive, base);
                close(lit_by(&glow, colour).base.base_color.to_linear(), base);
                if units.is_retail() {
                    close(lit_by(&lit_surface, colour).emissive, [multiple; 3]);
                    let light = white.point_light_in(units, Some(rgb)).unwrap();
                    close(light.color.to_linear(), [1.0; 3]);
                }
            }
        }
    }

    fn white_light() -> crate::world::database::LightRow {
        crate::world::database::LightRow {
            radius: 512.0,
            color: [255, 255, 255],
            flags: 0,
            fade: None,
            fade_status: None,
            falloff: None,
            reference_data: None,
            radius_status: None,
            flicker_period: None,
            flicker_intensity_amplitude: None,
            flicker_movement_amplitude: None,
            reference_never_fades: false,
        }
    }

    /// Under `skyrim` the source's colour is the mix of its day parts at the game hour, and the
    /// reference's light takes the record bytes times it. A reference without day parts (the
    /// `mudcrab` profile) keeps the day colour.
    #[test]
    fn the_day_parts_drive_the_surfaces_and_the_light_by_the_hour() {
        let parts = EmittanceDayParts([[200, 100, 0], [0, 0, 0], [100, 200, 40], [10, 20, 30]]);
        assert_eq!(parts.mixed([0.0, 1.0, 0.0, 0.0]), [0, 0, 0]);
        assert_eq!(parts.mixed([0.0, 0.2, 0.8, 0.0]), [80, 160, 32]);

        let mut app = App::new();
        // Sunrise 5:00 to 7:00, sunset 17:00 to 19:00.
        let catalog = crate::world::environment::EnvironmentCatalog::with_sun_for_tests(
            0x3C,
            0x15F,
            crate::sun::SunTiming([30, 42, 102, 114]),
            [None; 5],
        );
        app.insert_resource(catalog)
            .insert_resource(crate::config::EngineConfig {
                worldspace_id: 0x3C,
                ..default()
            })
            .insert_resource(crate::sun::GameClock {
                hour: 12.0,
                timescale: 0.0,
            })
            .add_systems(Update, drive_emittance_by_hour);
        let evaluation = crate::lights::evaluate_light(&white_light(), None);
        let light = app
            .world_mut()
            .spawn((
                evaluation
                    .point_light_in(LightUnits::Retail, Some([0, 0, 0]))
                    .unwrap(),
                crate::lights::SkyrimLight {
                    form_id: 1,
                    cell_id: 1,
                    shadow_flagged: false,
                },
            ))
            .id();
        let reference = app
            .world_mut()
            .spawn((
                parts,
                ReferenceEmittance::of(LightUnits::Mudcrab, [0, 0, 0]),
                evaluation,
            ))
            .add_child(light)
            .id();
        app.update();
        let colour = |app: &App| app.world().get::<PointLight>(light).unwrap().color;
        assert_eq!(colour(&app), Color::linear_rgb(0.0, 0.0, 0.0), "off by day");
        // 18:00 with these times: 0.8 sunset and 0.2 day.
        app.world_mut().resource_mut::<crate::sun::GameClock>().hour = 18.0;
        app.update();
        assert_eq!(
            *app.world().get::<ReferenceEmittance>(reference).unwrap(),
            ReferenceEmittance::of(LightUnits::Retail, [80, 160, 32])
        );
        assert_eq!(
            colour(&app),
            Color::linear_rgb(80.0 / 255.0, 160.0 / 255.0, 32.0 / 255.0)
        );
    }
}
