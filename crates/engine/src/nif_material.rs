//! Native construction of converted NIF materials using Bevy's glTF extension hooks.
use crate::nif_depth::{
    DECAL, DEPTH_TEST, DEPTH_WRITE, DYNAMIC_DECAL, NifDepthMaterial, NifDepthMaterialSource,
    NifDepthState, depth_material,
};
use bevy::{
    asset::LoadContext,
    gltf::{
        GltfMaterial,
        extensions::{ErasedGltfExtensionHandler, GltfExtensionHandler, GltfExtensionHandlers},
        gltf,
    },
    light::NotShadowCaster,
    prelude::*,
};
use std::collections::HashMap;

/// Register after `DefaultPlugins`; applies converter-tagged native material semantics.
pub struct NifMaterialPlugin;

impl Plugin for NifMaterialPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<NifDepthMaterial>::default())
            .register_type::<NifSourceMaterial>()
            .register_type::<NifDepthMaterialSource>();
        app.world_mut()
            .resource_mut::<GltfExtensionHandlers>()
            .0
            .write_blocking()
            .push(Box::new(NativeMaterialHandler::default()));
    }
}

#[derive(Clone, Default)]
struct NativeMaterialHandler {
    // Scene hooks use a child LoadContext, which cannot read the parent's material assets.
    // Bevy clones a handler per glTF load, so this cache also isolates concurrent loads.
    depth_states: HashMap<String, NifDepthState>,
}

/// The stock hook records this dependency before our hook replaces its render binding.
/// Keep its strong handle in the scene so scene-only loads remain fully loaded.
#[derive(Component, Clone, Reflect)]
#[reflect(Component)]
struct NifSourceMaterial(Handle<StandardMaterial>);

fn native_tag(material: &gltf::Material<'_>, name: &str, value: &str) -> bool {
    material
        .extras()
        .as_ref()
        .and_then(|extras| serde_json::from_str::<serde_json::Value>(extras.get()).ok())
        .is_some_and(|extras| extras["openSkyrim"][name].as_str() == Some(value))
}

fn has_normal_alpha_mask(material: &gltf::Material<'_>) -> bool {
    native_tag(material, "specularMask", "normal_alpha")
}

fn native_uv_transform(material: &gltf::Material<'_>) -> Option<bevy::math::Affine2> {
    let extras: serde_json::Value = serde_json::from_str(material.extras().as_ref()?.get()).ok()?;
    let transform = extras.pointer("/openSkyrim/uvTransform")?;
    let offset: [f32; 2] = serde_json::from_value(transform.get("offset")?.clone()).ok()?;
    let scale: [f32; 2] = serde_json::from_value(transform.get("scale")?.clone()).ok()?;
    if !offset.into_iter().chain(scale).all(f32::is_finite) {
        return None;
    }
    Some(bevy::math::Affine2::from_scale_angle_translation(
        scale.into(),
        0.0,
        offset.into(),
    ))
}

fn needs_native_material(material: &gltf::Material<'_>) -> bool {
    has_normal_alpha_mask(material)
        || native_tag(material, "normalConvention", "directx")
        || native_uv_transform(material).is_some()
        || native_depth_flags(material).is_some_and(|(flags_1, flags_2)| {
            flags_1 & (DECAL | DYNAMIC_DECAL) != 0
                || flags_1 & DEPTH_TEST == 0
                || flags_2 & DEPTH_WRITE == 0
        })
}

fn native_depth_flags(material: &gltf::Material<'_>) -> Option<(u32, u32)> {
    let extras: serde_json::Value = serde_json::from_str(material.extras().as_ref()?.get()).ok()?;
    let tag = extras.get("openSkyrim")?;
    let flags_1 = u32::try_from(tag.get("shaderFlags1")?.as_u64()?).ok()?;
    let flags_2 = u32::try_from(tag.get("shaderFlags2")?.as_u64()?).ok()?;
    Some((flags_1, flags_2))
}

fn native_depth_state(
    material: &gltf::Material<'_>,
    alpha_mode: AlphaMode,
) -> Option<NifDepthState> {
    let (flags_1, flags_2) = native_depth_flags(material)?;
    NifDepthState::from_flags(flags_1, flags_2, alpha_mode)
}

fn native_material(
    source: &gltf::Material<'_>,
    loaded: &StandardMaterial,
) -> Option<StandardMaterial> {
    if !needs_native_material(source) {
        return None;
    }
    let mut material = loaded.clone();
    if has_normal_alpha_mask(source) && material.specular_texture.is_some() {
        // Bevy 0.19 multiplies sampled mask alpha by 0.5 in pbr_fragment.wgsl.
        // Compensate only on this fresh native material, leaving valid glTF factors
        // and generic glTF assets unchanged. No mask (including pruning) means no doubling.
        material.reflectance *= 2.0;
    }
    if native_tag(source, "normalConvention", "directx") && material.normal_map_texture.is_some() {
        // Skyrim normal Y follows increasing texture V. Bevy's generated
        // bitangent uses the opposite orientation. Keep source RGB/alpha intact.
        material.flip_normal_map_y = true;
    }
    if let Some(transform) = native_uv_transform(source) {
        // NIF transforms every texture use, even with no diffuse texture from
        // which Bevy's stock glTF loader could recover the common transform.
        material.uv_transform = transform;
    }
    Some(material)
}

impl GltfExtensionHandler for NativeMaterialHandler {
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
        if !needs_native_material(source) {
            return;
        }
        // Reuse Bevy's fully constructed standard material instead of duplicating
        // its field conversion. This handler registers after the stock PBR handler.
        let standard = context
            .get_labeled(format!("{label}/std"))
            .and_then(|asset| asset.get::<StandardMaterial>())
            .expect("NifMaterialPlugin requires the stock PBR glTF handler before it");
        let material = native_material(source, standard).unwrap();
        if let Some(state) = native_depth_state(source, material.alpha_mode) {
            self.depth_states.insert(label.to_owned(), state);
            context.add_labeled_asset(
                format!("{label}/nif-depth"),
                depth_material(material.clone(), state),
            );
        }
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
        if needs_native_material(material) {
            let source = entity
                .get::<MeshMaterial3d<StandardMaterial>>()
                .expect("NifMaterialPlugin requires the stock PBR glTF handler before it")
                .0
                .clone();
            let base = context.get_label_handle::<StandardMaterial>(format!("{label}/nif"));
            entity.insert(NifSourceMaterial(source));
            if let Some(state) = self.depth_states.get(label).copied() {
                entity.remove::<MeshMaterial3d<StandardMaterial>>();
                entity.insert((
                    NifDepthMaterialSource(base),
                    MeshMaterial3d(
                        context.get_label_handle::<NifDepthMaterial>(format!("{label}/nif-depth")),
                    ),
                ));
                if state.excludes_shadow() {
                    entity.insert(NotShadowCaster);
                }
            } else {
                entity.insert(MeshMaterial3d(base));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_depth_requires_both_valid_flags_and_keeps_generic_gltf_unchanged() {
        for tag in [
            serde_json::json!({}),
            serde_json::json!({"shaderFlags1":DEPTH_TEST}),
            serde_json::json!({"shaderFlags1":-1,"shaderFlags2":0}),
            serde_json::json!({"shaderFlags1":4294967296_u64,"shaderFlags2":0}),
            serde_json::json!({"shaderFlags1":"0","shaderFlags2":0}),
            serde_json::json!({"shaderFlags1":DEPTH_TEST,"shaderFlags2":DEPTH_WRITE}),
        ] {
            let document = gltf::Gltf::from_slice(
                &serde_json::to_vec(&serde_json::json!({
                    "asset":{"version":"2.0"},"materials":[{"extras":{"openSkyrim":tag}}]
                }))
                .unwrap(),
            )
            .unwrap();
            let source = document.materials().next().unwrap();
            assert!(native_material(&source, &StandardMaterial::default()).is_none());
            assert!(native_depth_state(&source, AlphaMode::Opaque).is_none());
        }
    }

    #[test]
    fn depth_scene_clone_retains_one_render_binding_and_base_dependency() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_resource::<GltfExtensionHandlers>()
            .init_asset::<StandardMaterial>()
            .add_plugins(NifMaterialPlugin)
            .register_type::<MeshMaterial3d<NifDepthMaterial>>()
            .register_type::<NotShadowCaster>();
        let source = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial::default());
        let state = NifDepthState::from_flags(DEPTH_TEST | DECAL, 0, AlphaMode::Mask(0.5)).unwrap();
        let material = app
            .world_mut()
            .resource_mut::<Assets<NifDepthMaterial>>()
            .add(depth_material(StandardMaterial::default(), state));
        let mut world = World::new();
        world.spawn((
            NifSourceMaterial(source.clone()),
            NifDepthMaterialSource(source.clone()),
            MeshMaterial3d(material.clone()),
            NotShadowCaster,
        ));
        let mut cloned = WorldAsset::new(world)
            .clone_with(
                app.world()
                    .resource::<bevy::ecs::reflect::AppTypeRegistry>(),
            )
            .unwrap();
        let mut query = cloned.world.query::<(
            &NifDepthMaterialSource,
            &MeshMaterial3d<NifDepthMaterial>,
            Option<&MeshMaterial3d<StandardMaterial>>,
            Option<&NotShadowCaster>,
        )>();
        let (base, rendered, standard, no_shadow) = query.single(&cloned.world).unwrap();
        assert_eq!(base.0, source);
        assert_eq!(rendered.0, material);
        assert!(standard.is_none());
        assert!(no_shadow.is_some());
    }

    #[test]
    fn v15_native_uv_transform_works_without_a_diffuse_texture() {
        let document = gltf::Gltf::from_slice(
            br#"{"asset":{"version":"2.0"},"materials":[{
            "extras":{"openSkyrim":{"uvTransform":{"offset":[0.25,-0.5],"scale":[2,-3]}}}
        }]}"#,
        )
        .unwrap();
        let source = document.materials().next().unwrap();
        let loaded = StandardMaterial::default();
        let native = native_material(&source, &loaded).unwrap();
        assert_eq!(
            native.uv_transform.transform_point2(Vec2::new(0.5, 0.5)),
            Vec2::new(1.25, -2.0)
        );
        assert_eq!(loaded.uv_transform, bevy::math::Affine2::IDENTITY);
    }

    #[test]
    fn v13_native_normal_convention_is_scoped_and_preserves_mask() {
        for convention in [None, Some("directx"), Some("model_space")] {
            let document = gltf::Gltf::from_slice(
                serde_json::to_string(&serde_json::json!({
                    "asset":{"version":"2.0"}, "materials":[{
                        "extras":{"openSkyrim":{"normalConvention":convention}}
                    }]
                }))
                .unwrap()
                .as_bytes(),
            )
            .unwrap();
            let source = document.materials().next().unwrap();
            let loaded = StandardMaterial {
                normal_map_texture: Some(Handle::default()),
                specular_texture: Some(Handle::default()),
                reflectance: 0.4,
                ..default()
            };
            let native = native_material(&source, &loaded);
            if convention == Some("directx") {
                let native = native.expect("tagged tangent normals require native construction");
                assert!(native.flip_normal_map_y);
                assert_eq!(native.reflectance, loaded.reflectance);
                assert_eq!(native.specular_texture, loaded.specular_texture);
                assert_eq!(native.normal_map_texture, loaded.normal_map_texture);
            } else {
                assert!(native.is_none());
            }
            assert!(!loaded.flip_normal_map_y);
        }
    }

    #[test]
    fn v12_scene_cloning_retains_source_and_native_material_handles() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_resource::<GltfExtensionHandlers>()
            .add_plugins(NifMaterialPlugin)
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
                    let material = native_material(&source, &loaded);
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
