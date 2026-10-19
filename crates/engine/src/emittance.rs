//! Emission a reference's `XEMI` light or region lends the `External_Emittance` surfaces of its
//! model (#242).
//!
//! A NIF shader with `SLSF1_External_Emittance` takes its emission colour from outside the model:
//! from the light or region its reference names in `XEMI`. Building windows name regions whose
//! weather is black by day and amber at night, which is how Skyrim keeps windows dark in daylight.
//! The converter resolves every source's colour at each time of day into the `emittance` table
//! (`docs/specs/converters/db-schema.md`); the engine has one time of day, the day (`sky.rs`), and
//! multiplies the emission the model publishes by that day colour.
//!
//! A surface whose reference names no source, or one the converter could not resolve, keeps the
//! model's own emission.
use crate::nif_material::ExternalEmittance;
use bevy::{app::SceneSpawnerSystems, prelude::*};
use std::collections::HashMap;

/// Register after `NifMaterialPlugin`, which marks the surfaces this applies to.
pub struct EmittancePlugin;

impl Plugin for EmittancePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EmittanceMaterials>()
            .add_systems(
                SpawnScene,
                apply_reference_emittance.after(SceneSpawnerSystems::WorldInstanceSpawn),
            )
            .add_systems(Update, forget_freed_materials);
    }
}

/// The linear colour a reference's `XEMI` source lends its model's `External_Emittance` surfaces.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct ReferenceEmittance(pub LinearRgba);

impl ReferenceEmittance {
    /// The source's colour as the database stores it: 8-bit sRGB, like weather and light colours.
    pub fn from_srgb(rgb: [u8; 3]) -> Self {
        Self(Color::srgb_u8(rgb[0], rgb[1], rgb[2]).to_linear())
    }
}

/// The material a surface drew with before its reference's emittance was applied, so a second
/// application starts again from the model's own emission instead of compounding.
#[derive(Component, Clone)]
struct EmittanceSourceMaterial(Handle<StandardMaterial>);

/// One material per model material and emittance colour, shared by every reference that lends the
/// same colour, so emitting references keep batching together. The cache holds ids, not handles:
/// a derived material is freed with the last surface that draws with it, as a cell unloads, and
/// [`forget_freed_materials`] then drops its entry.
#[derive(Resource, Default)]
struct EmittanceMaterials(
    HashMap<(AssetId<StandardMaterial>, [u32; 3]), AssetId<StandardMaterial>>,
);

/// `material` with its emission multiplied by `colour`. The published emission is the NIF's tint
/// times its multiplier (`docs/specs/engine/color-pipeline.md`), which the glTF cannot separate, so
/// the external colour multiplies the tint rather than replacing it; the window shaders that use
/// external emittance have a white tint, where the two agree.
fn lit_by(material: &StandardMaterial, colour: LinearRgba) -> StandardMaterial {
    let emissive = material.emissive;
    StandardMaterial {
        emissive: LinearRgba::new(
            emissive.red * colour.red,
            emissive.green * colour.green,
            emissive.blue * colour.blue,
            emissive.alpha,
        ),
        ..material.clone()
    }
}

/// A model's `External_Emittance` surfaces: the material each draws with and, once a reference's
/// colour has been applied, the model's own material it started from.
type ExternalEmittanceSurfaces<'world, 'state> = Query<
    'world,
    'state,
    (
        Entity,
        &'static mut MeshMaterial3d<StandardMaterial>,
        Option<&'static EmittanceSourceMaterial>,
    ),
    With<ExternalEmittance>,
>;

/// Marks a reference with a surface whose model material had not loaded when its scene was
/// instantiated, so [`apply_reference_emittance`] tries again once a material arrives.
#[derive(Component)]
struct AwaitingEmittanceMaterial;

/// Emitting references to visit: a newly instantiated model, or one still waiting for a material.
type EmittingReferences<'world, 'state> = Query<
    'world,
    'state,
    (
        Entity,
        &'static ReferenceEmittance,
        Ref<'static, Children>,
        Has<AwaitingEmittanceMaterial>,
    ),
    Or<(Changed<Children>, With<AwaitingEmittanceMaterial>)>,
>;

/// Gives every `External_Emittance` surface of a newly instantiated model its reference's colour.
///
/// A reference gains a child the moment Bevy writes its converted scene below it, so
/// `Changed<Children>` on an emitting reference is the frame its surfaces exist. A model can be
/// instantiated before its materials finish loading; a surface whose material is not there yet is
/// left as it is and its reference waits, to be visited again when a material is added.
fn apply_reference_emittance(
    mut commands: Commands,
    references: EmittingReferences,
    descendants: Query<&Children>,
    mut surfaces: ExternalEmittanceSurfaces,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut cache: ResMut<EmittanceMaterials>,
    mut material_events: MessageReader<AssetEvent<StandardMaterial>>,
) {
    let material_arrived = material_events.read().any(|event| {
        matches!(
            event,
            AssetEvent::Added { .. } | AssetEvent::LoadedWithDependencies { .. }
        )
    });
    for (reference, emittance, children, awaiting) in &references {
        if !children.is_changed() && !material_arrived {
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
                commands.entity(reference).insert(AwaitingEmittanceMaterial);
            }
            (false, true) => {
                commands
                    .entity(reference)
                    .remove::<AwaitingEmittanceMaterial>();
            }
            _ => {}
        }
    }
}

/// Drops the cache entries of materials Bevy has freed: a derived material whose last surface
/// unloaded, or a model material whose model did.
fn forget_freed_materials(
    mut events: MessageReader<AssetEvent<StandardMaterial>>,
    mut cache: ResMut<EmittanceMaterials>,
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
            .init_resource::<EmittanceMaterials>()
            .add_systems(
                Update,
                (apply_reference_emittance, forget_freed_materials).chain(),
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
            reference.insert(ReferenceEmittance::from_srgb(rgb));
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
}
