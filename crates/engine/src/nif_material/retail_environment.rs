//! Binds the environment cube and mask of a retail environment surface once they have loaded
//! (SPEC-1 section 9.2, technique 1;
//! [near-lighting.md](../../../../docs/specs/engine/near-lighting.md)).
//!
//! The loader only names the two images ([`StandardMaterial::retail_environment_cube_source`]).
//! A cube slot that holds another shape cannot be bound as a cube, and the glTF loader decodes
//! both images as sRGB while retail samples the stored bytes. So an image is bound here, after
//! its shape is checked and its sRGB view is replaced. A surface whose images cannot be used
//! draws without a reflection.

use crate::nif_depth::NifDepthMaterial;
use bevy::{
    image::Image,
    platform::collections::HashSet,
    prelude::*,
    render::render_resource::{TextureDimension, TextureViewDimension},
};

/// A material that holds a retail surface with environment sources.
pub(super) trait EnvironmentSurface: Asset {
    fn surface(&self) -> &StandardMaterial;
    fn surface_mut(&mut self) -> &mut StandardMaterial;
}

impl EnvironmentSurface for StandardMaterial {
    fn surface(&self) -> &StandardMaterial {
        self
    }

    fn surface_mut(&mut self) -> &mut StandardMaterial {
        self
    }
}

impl EnvironmentSurface for NifDepthMaterial {
    fn surface(&self) -> &StandardMaterial {
        &self.base
    }

    fn surface_mut(&mut self) -> &mut StandardMaterial {
        &mut self.base
    }
}

/// Registers the binding systems. Only the `skyrim` profile names environment sources.
pub(super) fn build(app: &mut App) {
    app.add_systems(
        Last,
        (
            bind::<StandardMaterial>.run_if(resource_exists::<Assets<StandardMaterial>>),
            bind::<NifDepthMaterial>.run_if(resource_exists::<Assets<NifDepthMaterial>>),
        ),
    );
}

/// What an image is to the shader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Cube,
    Mask,
}

#[derive(Debug, PartialEq, Eq)]
enum Prepared {
    Loading,
    Ready,
    Unusable(&'static str),
}

/// Checks the shape of a loaded image for `role` and makes its view read the stored bytes.
fn prepare(images: &mut Assets<Image>, handle: &Handle<Image>, role: Role) -> Prepared {
    let Some(image) = images.get(handle) else {
        return Prepared::Loading;
    };
    let descriptor = &image.texture_descriptor;
    let view = image
        .texture_view_descriptor
        .as_ref()
        .and_then(|view| view.dimension);
    let layers = descriptor.size.depth_or_array_layers;
    let fits = descriptor.dimension == TextureDimension::D2
        && match role {
            Role::Cube => {
                layers == 6
                    && descriptor.size.width == descriptor.size.height
                    && view == Some(TextureViewDimension::Cube)
            }
            Role::Mask => layers == 1 && view.is_none_or(|view| view == TextureViewDimension::D2),
        };
    if !fits {
        return Prepared::Unusable(match role {
            Role::Cube => "the environment cube slot does not hold a cube map",
            Role::Mask => "the environment mask slot does not hold a flat image",
        });
    }
    let format = descriptor.format;
    if !format.is_srgb() {
        return Prepared::Ready;
    }
    if image.data.is_none() {
        return Prepared::Unusable("the image has an sRGB view and no data to upload again");
    }
    if let Some(mut image) = images.get_mut(handle) {
        image.texture_descriptor.format = format.remove_srgb_suffix();
        if let Some(view) = image.texture_view_descriptor.as_mut() {
            view.format = None;
        }
    }
    Prepared::Ready
}

fn bind<M: EnvironmentSurface>(
    mut events: MessageReader<AssetEvent<M>>,
    mut pending: Local<HashSet<AssetId<M>>>,
    mut materials: ResMut<Assets<M>>,
    mut images: ResMut<Assets<Image>>,
) {
    for event in events.read() {
        if let AssetEvent::Added { id } | AssetEvent::Modified { id } = event {
            pending.insert(*id);
        }
    }
    pending.retain(|id| {
        let Some(surface) = materials.get(*id).map(EnvironmentSurface::surface) else {
            return false;
        };
        let Some(cube) = surface.retail_environment_cube_source.clone() else {
            return false;
        };
        if surface.retail_environment_cube.as_ref() == Some(&cube) {
            return false;
        }
        let mask = surface.retail_environment_mask_source.clone();
        let cube_state = prepare(&mut images, &cube, Role::Cube);
        let mask_state = mask
            .as_ref()
            .map(|mask| prepare(&mut images, mask, Role::Mask));
        if cube_state == Prepared::Loading || mask_state == Some(Prepared::Loading) {
            return true;
        }
        if let Prepared::Unusable(reason) = cube_state {
            warn_once!("environment surface drawn without its reflection: {reason}");
            return false;
        }
        let mask = match mask_state {
            Some(Prepared::Unusable(reason)) => {
                warn_once!("environment surface drawn without its mask: {reason}");
                None
            }
            _ => mask,
        };
        if let Some(mut material) = materials.get_mut(*id) {
            let surface = material.surface_mut();
            surface.retail_environment_cube = Some(cube);
            surface.retail_environment_mask = mask;
        }
        false
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::{
        asset::AssetPlugin,
        render::render_resource::{Extent3d, TextureFormat, TextureViewDescriptor},
    };

    fn image(layers: u32, cube_view: bool, format: TextureFormat) -> Image {
        let mut image = Image::new_fill(
            Extent3d {
                width: 2,
                height: 2,
                depth_or_array_layers: layers,
            },
            TextureDimension::D2,
            &[128, 64, 32, 255],
            format,
            default(),
        );
        if cube_view {
            image.texture_view_descriptor = Some(TextureViewDescriptor {
                dimension: Some(TextureViewDimension::Cube),
                ..default()
            });
        }
        image
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Image>()
            .init_asset::<StandardMaterial>();
        build(&mut app);
        app
    }

    fn add(app: &mut App, image: Image) -> Handle<Image> {
        app.world_mut().resource_mut::<Assets<Image>>().add(image)
    }

    fn surface(
        app: &mut App,
        cube: Handle<Image>,
        mask: Option<Handle<Image>>,
    ) -> Handle<StandardMaterial> {
        app.world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial {
                retail_environment_cube_source: Some(cube),
                retail_environment_mask_source: mask,
                ..default()
            })
    }

    fn bound(app: &App, material: &Handle<StandardMaterial>) -> (bool, bool) {
        let material = app
            .world()
            .resource::<Assets<StandardMaterial>>()
            .get(material)
            .unwrap();
        (
            material.retail_environment_cube.is_some(),
            material.retail_environment_mask.is_some(),
        )
    }

    #[test]
    fn a_loaded_cube_and_mask_bind_with_their_stored_bytes() {
        let mut app = app();
        let cube = add(&mut app, image(6, true, TextureFormat::Rgba8UnormSrgb));
        let mask = add(&mut app, image(1, false, TextureFormat::Rgba8UnormSrgb));
        let material = surface(&mut app, cube.clone(), Some(mask.clone()));
        app.update();
        app.update();
        assert_eq!(bound(&app, &material), (true, true));
        let images = app.world().resource::<Assets<Image>>();
        for handle in [&cube, &mask] {
            assert_eq!(
                images.get(handle).unwrap().texture_descriptor.format,
                TextureFormat::Rgba8Unorm
            );
        }
    }

    #[test]
    fn a_surface_waits_for_its_images() {
        let mut app = app();
        let reserved = app.world().resource::<Assets<Image>>().reserve_handle();
        let material = surface(&mut app, reserved.clone(), None);
        app.update();
        app.update();
        assert_eq!(bound(&app, &material), (false, false));
        app.world_mut()
            .resource_mut::<Assets<Image>>()
            .insert(reserved.id(), image(6, true, TextureFormat::Rgba8Unorm))
            .unwrap();
        app.update();
        assert_eq!(bound(&app, &material), (true, false));
    }

    #[test]
    fn a_flat_image_in_the_cube_slot_is_never_bound() {
        let mut app = app();
        let flat = add(&mut app, image(1, false, TextureFormat::Rgba8Unorm));
        // Six layers without the cube view are an array, not a cube.
        let array = add(&mut app, image(6, false, TextureFormat::Rgba8Unorm));
        let cube = add(&mut app, image(6, true, TextureFormat::Rgba8Unorm));
        let first = surface(&mut app, flat, None);
        let second = surface(&mut app, array, None);
        // A cube in the mask slot drops the mask only.
        let third = surface(&mut app, cube.clone(), Some(cube));
        app.update();
        app.update();
        assert_eq!(bound(&app, &first), (false, false));
        assert_eq!(bound(&app, &second), (false, false));
        assert_eq!(bound(&app, &third), (true, false));
    }
}
