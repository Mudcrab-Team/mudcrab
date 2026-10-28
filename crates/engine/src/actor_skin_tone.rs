//! An actor's skin tone (`NPC_/QNAM`) on the `skin_tint` surfaces of its body, hands and feet.
//!
//! The game hands each NPC's skin tone to the body's lighting shader as its tint colour, which
//! blends it into the diffuse ([`crate::skin_material`], `SKIN_TINT`). The converted body GLB is
//! shared by every actor of a sex and its labelled materials by every instance of the GLB, so the
//! tone cannot live in the loaded material. Instead each tinted surface below an actor is switched
//! to a material derived from its own with the actor's tone set, the way
//! [`crate::emittance`] derives a reference's emissive materials.
//!
//! Derived materials are shared per (model material, tone), so actors with the same tone keep
//! batching together, and the loaded scene is never changed: streaming preparation and readiness
//! read the scene asset and the native base material (`NifNativeMaterialSource`), which keep the
//! model's own material, and a derived material reuses that material's images.
//!
//! An actor without a tone keeps the model's material with `SKIN_TINT` clear.
use crate::skin_material::{SkinTintSurface, SkyrimSkinMaterial};
use bevy::{app::SceneSpawnerSystems, prelude::*};
use std::collections::HashMap;

/// Register after `NifMaterialPlugin`, which marks the surfaces this applies to.
pub struct ActorSkinTonePlugin;

impl Plugin for ActorSkinTonePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SkinToneMaterials>()
            .add_systems(
                SpawnScene,
                apply_actor_skin_tone.after(SceneSpawnerSystems::WorldInstanceSpawn),
            )
            .add_systems(Update, forget_freed_materials);
    }
}

/// An actor's body skin tone: `NPC_/QNAM` as RGB in 0..1 on the encoded scale the record stores.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct ActorSkinTone(pub Vec3);

impl ActorSkinTone {
    /// A tone from a catalog row's three channels; `None` unless all are finite.
    pub fn new(rgb: [f32; 3]) -> Option<Self> {
        let tone = Vec3::from_array(rgb);
        tone.is_finite().then_some(Self(tone))
    }
}

/// The material a surface drew with before its actor's tone was applied, so a later application
/// starts from the model's material instead of a derived one.
#[derive(Component, Clone)]
struct SkinToneSourceMaterial(Handle<SkyrimSkinMaterial>);

/// One material per model material and tone. The cache holds ids, not handles: a derived material
/// is freed with the last surface that draws with it, as a cell unloads, and
/// [`forget_freed_materials`] then drops its entry.
#[derive(Resource, Default)]
struct SkinToneMaterials(
    HashMap<(AssetId<SkyrimSkinMaterial>, [u32; 3]), AssetId<SkyrimSkinMaterial>>,
);

/// `material` with the skin tint set to `tone`.
fn toned(material: &SkyrimSkinMaterial, tone: Vec3) -> SkyrimSkinMaterial {
    let mut material = material.clone();
    material.extension.settings.set_skin_tint(tone);
    material
}

type SkinTintSurfaces<'world, 'state> = Query<
    'world,
    'state,
    (
        Entity,
        &'static mut MeshMaterial3d<SkyrimSkinMaterial>,
        Option<&'static SkinToneSourceMaterial>,
    ),
    With<SkinTintSurface>,
>;

/// Marks an actor with a surface whose model material had not loaded when its scene was
/// instantiated, so [`apply_actor_skin_tone`] tries again once a material arrives.
#[derive(Component)]
struct AwaitingSkinToneMaterial;

/// Toned actors to visit: a newly instantiated model or head, or one still waiting for a material.
type TonedActors<'world, 'state> = Query<
    'world,
    'state,
    (
        Entity,
        &'static ActorSkinTone,
        Ref<'static, Children>,
        Has<AwaitingSkinToneMaterial>,
    ),
    Or<(Changed<Children>, With<AwaitingSkinToneMaterial>)>,
>;

/// Gives every `skin_tint` surface below a newly instantiated actor model its actor's tone.
///
/// An actor gains a child the moment Bevy writes its converted scene below it, so
/// `Changed<Children>` on a toned actor is the frame its surfaces exist. A surface whose material
/// has not loaded yet is left as it is and its actor waits for a material to arrive.
fn apply_actor_skin_tone(
    mut commands: Commands,
    actors: TonedActors,
    descendants: Query<&Children>,
    mut surfaces: SkinTintSurfaces,
    mut materials: ResMut<Assets<SkyrimSkinMaterial>>,
    mut cache: ResMut<SkinToneMaterials>,
    mut material_events: MessageReader<AssetEvent<SkyrimSkinMaterial>>,
) {
    let material_arrived = material_events.read().any(|event| {
        matches!(
            event,
            AssetEvent::Added { .. } | AssetEvent::LoadedWithDependencies { .. }
        )
    });
    for (actor, tone, children, awaiting) in &actors {
        if !children.is_changed() && !material_arrived {
            continue;
        }
        let mut missing = false;
        let tone = tone.0;
        let tone_key = tone.to_array().map(f32::to_bits);
        for descendant in descendants.iter_descendants(actor) {
            let Ok((surface, mut material, source)) = surfaces.get_mut(descendant) else {
                continue;
            };
            let source = match source {
                Some(source) => source.0.clone(),
                None => {
                    commands
                        .entity(surface)
                        .insert(SkinToneSourceMaterial(material.0.clone()));
                    material.0.clone()
                }
            };
            let key = (source.id(), tone_key);
            let cached = cache
                .0
                .get(&key)
                .and_then(|&derived| materials.get_strong_handle(derived));
            let derived = match cached {
                Some(derived) => derived,
                None => {
                    let Some(derived) = materials.get(&source).map(|base| toned(base, tone)) else {
                        missing = true;
                        continue;
                    };
                    let derived = materials.add(derived);
                    cache.0.insert(key, derived.id());
                    derived
                }
            };
            if material.0 != derived {
                material.0 = derived;
            }
        }
        match (missing, awaiting) {
            (true, false) => {
                commands.entity(actor).insert(AwaitingSkinToneMaterial);
            }
            (false, true) => {
                commands.entity(actor).remove::<AwaitingSkinToneMaterial>();
            }
            _ => {}
        }
    }
}

/// Drops the cache entries of materials Bevy has freed.
fn forget_freed_materials(
    mut events: MessageReader<AssetEvent<SkyrimSkinMaterial>>,
    mut cache: ResMut<SkinToneMaterials>,
) {
    for event in events.read() {
        if let AssetEvent::Removed { id } = event {
            cache
                .0
                .retain(|(source, _), derived| source != id && derived != id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skin_material::SkyrimSkinFlags;

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<SkyrimSkinMaterial>()
            .init_resource::<SkinToneMaterials>()
            .add_systems(
                Update,
                (apply_actor_skin_tone, forget_freed_materials).chain(),
            );
        app
    }

    fn skin(app: &mut App) -> Handle<SkyrimSkinMaterial> {
        let mut material = SkyrimSkinMaterial::default();
        material.extension.settings.flags = SkyrimSkinFlags::MODEL_SPACE_NORMALS;
        app.world_mut()
            .resource_mut::<Assets<SkyrimSkinMaterial>>()
            .add(material)
    }

    fn handle(app: &App, entity: Entity) -> Handle<SkyrimSkinMaterial> {
        app.world()
            .get::<MeshMaterial3d<SkyrimSkinMaterial>>(entity)
            .unwrap()
            .0
            .clone()
    }

    fn settings(app: &App, entity: Entity) -> crate::skin_material::SkyrimSkinSettings {
        app.world()
            .resource::<Assets<SkyrimSkinMaterial>>()
            .get(&handle(app, entity))
            .unwrap()
            .extension
            .settings
            .clone()
    }

    /// An actor with a model below it: one `skin_tint` surface and one untinted skin surface (a
    /// FaceGen head, say) sharing its material.
    fn actor(
        app: &mut App,
        tone: Option<[f32; 3]>,
        material: &Handle<SkyrimSkinMaterial>,
    ) -> (Entity, Entity, Entity) {
        let world = app.world_mut();
        let body = world
            .spawn((MeshMaterial3d(material.clone()), SkinTintSurface))
            .id();
        let head = world.spawn(MeshMaterial3d(material.clone())).id();
        let root = world.spawn_empty().add_children(&[body, head]).id();
        let mut actor = world.spawn_empty();
        if let Some(tone) = tone.and_then(ActorSkinTone::new) {
            actor.insert(tone);
        }
        let actor = actor.add_child(root).id();
        (actor, body, head)
    }

    const ALVOR: [f32; 3] = [198.0 / 255.0, 176.0 / 255.0, 168.0 / 255.0];

    #[test]
    fn the_tone_reaches_only_the_skin_tint_surface() {
        let mut app = app();
        let material = skin(&mut app);
        let (_, body, head) = actor(&mut app, Some(ALVOR), &material);
        app.update();
        let body_settings = settings(&app, body);
        assert_ne!(body_settings.flags & SkyrimSkinFlags::SKIN_TINT, 0);
        assert_eq!(
            body_settings.tint_color,
            Vec3::from_array(ALVOR).extend(1.0)
        );
        assert_eq!(
            handle(&app, head),
            material,
            "the head keeps the model's material"
        );
        let model = app
            .world()
            .resource::<Assets<SkyrimSkinMaterial>>()
            .get(&material)
            .unwrap();
        assert_eq!(
            model.extension.settings.flags & SkyrimSkinFlags::SKIN_TINT,
            0,
            "the shared material is not changed in place"
        );
    }

    #[test]
    fn actors_share_a_material_per_tone() {
        let mut app = app();
        let material = skin(&mut app);
        let (_, first, _) = actor(&mut app, Some(ALVOR), &material);
        let (_, second, _) = actor(&mut app, Some(ALVOR), &material);
        let (_, other, _) = actor(&mut app, Some([0.866_667; 3]), &material);
        app.update();
        assert_eq!(handle(&app, first), handle(&app, second));
        assert_ne!(handle(&app, first), handle(&app, other));
        assert_ne!(handle(&app, first), material);
    }

    #[test]
    fn an_actor_without_a_tone_keeps_the_model_material() {
        let mut app = app();
        let material = skin(&mut app);
        let (_, body, _) = actor(&mut app, None, &material);
        app.update();
        assert_eq!(handle(&app, body), material);
        assert!(ActorSkinTone::new([0.5, f32::NAN, 0.5]).is_none());
    }

    #[test]
    fn a_later_change_below_the_actor_starts_again_from_the_model_material() {
        let mut app = app();
        let material = skin(&mut app);
        let (actor, body, _) = actor(&mut app, Some(ALVOR), &material);
        app.update();
        let derived = handle(&app, body);
        let extra = app.world_mut().spawn_empty().id();
        app.world_mut().entity_mut(actor).add_child(extra);
        app.update();
        assert_eq!(handle(&app, body), derived, "no second derivation");
    }

    #[test]
    fn a_surface_whose_material_loads_late_gets_the_tone_when_it_arrives() {
        let mut app = app();
        let late = app
            .world()
            .resource::<Assets<SkyrimSkinMaterial>>()
            .reserve_handle();
        let (placed, body, _) = actor(&mut app, Some(ALVOR), &late);
        app.update();
        assert_eq!(handle(&app, body), late);
        assert!(
            app.world()
                .get::<AwaitingSkinToneMaterial>(placed)
                .is_some()
        );
        app.world_mut()
            .resource_mut::<Assets<SkyrimSkinMaterial>>()
            .insert(&late, SkyrimSkinMaterial::default())
            .unwrap();
        for _ in 0..3 {
            app.update();
        }
        assert_ne!(settings(&app, body).flags & SkyrimSkinFlags::SKIN_TINT, 0);
        assert!(
            app.world()
                .get::<AwaitingSkinToneMaterial>(placed)
                .is_none()
        );
    }

    #[test]
    fn a_derived_material_is_freed_with_its_last_surface() {
        let mut app = app();
        let material = skin(&mut app);
        let (placed, body, _) = actor(&mut app, Some(ALVOR), &material);
        app.update();
        let derived = handle(&app, body).id();
        app.world_mut().entity_mut(placed).despawn();
        for _ in 0..3 {
            app.update();
        }
        assert!(
            !app.world()
                .resource::<Assets<SkyrimSkinMaterial>>()
                .contains(derived)
        );
        assert!(app.world().resource::<SkinToneMaterials>().0.is_empty());
    }
}
