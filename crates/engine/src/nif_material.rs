//! Native construction of converted NIF materials using Bevy's glTF extension hooks.
use bevy::{
    asset::LoadContext,
    gltf::{
        GltfMaterial,
        extensions::{ErasedGltfExtensionHandler, GltfExtensionHandler, GltfExtensionHandlers},
        gltf,
    },
    prelude::*,
};

/// Register after `DefaultPlugins`; only converter-tagged normal-alpha masks use this handler.
pub struct NifSpecularPlugin;

impl Plugin for NifSpecularPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<NifSourceMaterial>();
        app.world_mut()
            .resource_mut::<GltfExtensionHandlers>()
            .0
            .write_blocking()
            .push(Box::new(NormalAlphaMask));
    }
}

#[derive(Clone)]
struct NormalAlphaMask;

/// The stock hook records this dependency before our hook replaces its render binding.
/// Keep its strong handle in the scene so scene-only loads remain fully loaded.
#[derive(Component, Clone, Reflect)]
#[reflect(Component)]
struct NifSourceMaterial(Handle<StandardMaterial>);

fn has_normal_alpha_mask(material: &gltf::Material<'_>) -> bool {
    material
        .extras()
        .as_ref()
        .and_then(|extras| serde_json::from_str::<serde_json::Value>(extras.get()).ok())
        .is_some_and(|extras| {
            extras
                .pointer("/openSkyrim/specularMask")
                .and_then(serde_json::Value::as_str)
                == Some("normal_alpha")
        })
}

fn native_mask_material(
    source: &gltf::Material<'_>,
    loaded: &StandardMaterial,
) -> Option<StandardMaterial> {
    if !has_normal_alpha_mask(source) {
        return None;
    }
    let mut material = loaded.clone();
    if material.specular_texture.is_some() {
        // Bevy 0.19 multiplies sampled mask alpha by 0.5 in pbr_fragment.wgsl.
        // Compensate only on this fresh native material, leaving valid glTF factors
        // and generic glTF assets unchanged. No mask (including pruning) means no doubling.
        material.reflectance *= 2.0;
    }
    Some(material)
}

impl GltfExtensionHandler for NormalAlphaMask {
    fn dyn_clone(&self) -> Box<dyn ErasedGltfExtensionHandler> {
        Box::new(self.clone())
    }

    fn on_material(
        &mut self,
        context: &mut LoadContext<'_>,
        source: &gltf::Material<'_>,
        _handle: Handle<GltfMaterial>,
        _loaded: &GltfMaterial,
        label: &str,
    ) {
        if !has_normal_alpha_mask(source) {
            return;
        }
        // Reuse Bevy's fully constructed standard material instead of duplicating
        // its field conversion. This handler registers after the stock PBR handler.
        let standard = context
            .get_labeled(format!("{label}/std"))
            .and_then(|asset| asset.get::<StandardMaterial>())
            .expect("NifSpecularPlugin requires the stock PBR glTF handler before it");
        let material = native_mask_material(source, standard).unwrap();
        context.add_labeled_asset(format!("{label}/nif"), material);
    }

    fn on_spawn_mesh_and_material(
        &mut self,
        context: &mut LoadContext<'_>,
        _primitive: &gltf::Primitive<'_>,
        _mesh: &gltf::Mesh<'_>,
        material: &gltf::Material<'_>,
        entity: &mut EntityWorldMut,
        label: &str,
    ) {
        if has_normal_alpha_mask(material) {
            let source = entity
                .get::<MeshMaterial3d<StandardMaterial>>()
                .expect("NifSpecularPlugin requires the stock PBR glTF handler before it")
                .0
                .clone();
            entity.insert((
                NifSourceMaterial(source),
                MeshMaterial3d(
                    context.get_label_handle::<StandardMaterial>(format!("{label}/nif")),
                ),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v12_scene_cloning_retains_source_and_native_material_handles() {
        let mut app = App::new();
        app.init_resource::<GltfExtensionHandlers>()
            .add_plugins(NifSpecularPlugin)
            .register_type::<MeshMaterial3d<StandardMaterial>>();
        let mut assets = Assets::<StandardMaterial>::default();
        let source = assets.add(StandardMaterial::default());
        let native = assets.add(StandardMaterial::default());
        let mut world = World::new();
        world.spawn((
            NifSourceMaterial(source.clone()),
            MeshMaterial3d(native.clone()),
        ));
        let mut cloned = WorldAsset::new(world)
            .clone_with(
                app.world()
                    .resource::<bevy::ecs::reflect::AppTypeRegistry>(),
            )
            .unwrap();
        let mut query = cloned
            .world
            .query::<(&NifSourceMaterial, &MeshMaterial3d<StandardMaterial>)>();
        let (retained, rendered) = query.single(&cloned.world).unwrap();
        assert_eq!(retained.0, source);
        assert_eq!(rendered.0, native);
    }

    #[test]
    fn v10_native_compensation_requires_tag_and_loaded_mask_and_does_not_compound() {
        for tagged in [false, true] {
            let document = gltf::Gltf::from_slice(serde_json::to_string(&serde_json::json!({
                "asset":{"version":"2.0"},"materials":[{
                    "extras":if tagged { serde_json::json!({"openSkyrim":{"specularMask":"normal_alpha"}}) }
                             else { serde_json::json!({}) }
                }]
            })).unwrap().as_bytes()).unwrap();
            let source = document.materials().next().unwrap();
            for mask in [false, true] {
                let loaded = StandardMaterial {
                    reflectance: 0.4,
                    specular_texture: mask.then(Handle::default),
                    ..default()
                };
                for _ in 0..2 {
                    let material = native_mask_material(&source, &loaded);
                    if tagged {
                        assert!(
                            (material.unwrap().reflectance - if mask { 0.8 } else { 0.4 }).abs()
                                < 1e-6
                        );
                    } else {
                        assert!(material.is_none());
                    }
                    assert_eq!(loaded.reflectance, 0.4);
                }
            }
        }
    }
}
