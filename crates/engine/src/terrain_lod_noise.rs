use bevy::{
    asset::{LoadState, embedded_asset},
    ecs::system::SystemParam,
    image::{
        ImageAddressMode, ImageFilterMode, ImageLoaderSettings, ImageSampler,
        ImageSamplerDescriptor,
    },
    pbr::{ExtendedMaterial, MaterialExtension, MaterialPlugin},
    prelude::*,
    shader::ShaderRef,
};

use crate::{config::EngineConfig, visual_profile::VisualProfile};

pub(crate) type NoisyTerrainLodMaterial = ExtendedMaterial<StandardMaterial, LandNoise>;

#[derive(Asset, TypePath, Clone, bevy::render::render_resource::AsBindGroup)]
pub(crate) struct LandNoise {
    #[texture(100)]
    #[sampler(101)]
    pub image: Handle<Image>,
}

impl MaterialExtension for LandNoise {
    fn fragment_shader() -> ShaderRef {
        "embedded://engine/shaders/terrain_lod_noise.wgsl".into()
    }
}

#[derive(Resource)]
struct LandNoiseImage(Handle<Image>);

impl LandNoiseImage {
    fn loaded(&self, server: &AssetServer) -> Option<&Handle<Image>> {
        matches!(server.get_load_state(self.0.id()), Some(LoadState::Loaded)).then_some(&self.0)
    }

    fn loading(&self, server: &AssetServer) -> bool {
        matches!(
            server.get_load_state(self.0.id()),
            Some(LoadState::Loading | LoadState::NotLoaded)
        )
    }
}

#[derive(SystemParam)]
pub(crate) struct LandNoiseMaterials<'w> {
    image: Option<Res<'w, LandNoiseImage>>,
    pub materials: Option<ResMut<'w, Assets<NoisyTerrainLodMaterial>>>,
}

impl LandNoiseMaterials<'_> {
    pub fn loading(&self, server: &AssetServer) -> bool {
        self.image
            .as_ref()
            .is_some_and(|image| image.loading(server))
    }

    pub fn loaded(&self, server: &AssetServer) -> Option<Handle<Image>> {
        self.image.as_ref()?.loaded(server).cloned()
    }
}

pub(crate) struct TerrainLodNoisePlugin;

impl Plugin for TerrainLodNoisePlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "shaders/terrain_lod_noise.wgsl");
        app.add_plugins(MaterialPlugin::<NoisyTerrainLodMaterial>::default())
            .add_systems(Startup, load_land_noise);
    }
}

fn load_land_noise(mut commands: Commands, config: Res<EngineConfig>, server: Res<AssetServer>) {
    if config.visual_profile != VisualProfile::Skyrim {
        return;
    }
    // A shared texture role for all terrain LOD chunks.
    let image = server
        .load_builder()
        .with_settings(|settings: &mut ImageLoaderSettings| {
            settings.is_srgb = false;
            settings.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                address_mode_u: ImageAddressMode::Repeat,
                address_mode_v: ImageAddressMode::Repeat,
                mag_filter: ImageFilterMode::Linear,
                min_filter: ImageFilterMode::Linear,
                lod_max_clamp: 0.0,
                ..default()
            });
        })
        .load("textures/terrain/noise.ktx2");
    commands.insert_resource(LandNoiseImage(image));
}

#[cfg(test)]
mod tests {
    use wgpu::naga::{Expression, Literal, front::wgsl};

    // Evaluate the production equation through WGSL's constant evaluator. Keep the test
    // independent of graphics hardware and use the shader's coefficients directly.
    fn multiplier(base: f32, red: f32) -> f64 {
        let shader = include_str!("shaders/terrain_lod_noise.wgsl");
        let (_, function) = shader
            .split_once("fn noise_multiplier(")
            .expect("noise function");
        let (_, body) = function.split_once('{').expect("function body");
        let (body, _) = body.split_once('}').expect("function end");
        let equations = body
            .replace("let ", "const ")
            .replace("return ", "const multiplier = ");
        let module = wgsl::parse_str(&format!(
            "const base = vec3<f32>({base}); const red: f32 = {red}; {equations}"
        ))
        .expect("constant evaluation of land noise");
        let (_, result) = module
            .constants
            .iter()
            .find(|(_, item)| item.name.as_deref() == Some("multiplier"))
            .expect("multiplier constant");
        match module.global_expressions[result.init] {
            Expression::Literal(Literal::F32(value)) => f64::from(value),
            ref other => panic!("expected a folded f32, got {other:?}"),
        }
    }

    #[test]
    fn land_noise_uses_red_and_reduces_its_strength_on_brighter_terrain() {
        for (base, red, expected) in [
            (0.0, 0.0, 0.37),
            (0.0, 1.0, 1.203333),
            (1.0, 0.0, 0.616667),
            (1.0, 1.0, 0.783333),
        ] {
            assert!((multiplier(base, red) - expected).abs() < 0.000002);
        }
    }
}
