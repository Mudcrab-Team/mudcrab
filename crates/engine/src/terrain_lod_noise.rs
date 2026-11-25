use bevy::{
    asset::{LoadState, embedded_asset},
    ecs::system::SystemParam,
    image::{
        ImageAddressMode, ImageFilterMode, ImageLoaderSettings, ImageSampler,
        ImageSamplerDescriptor,
    },
    pbr::{ExtendedMaterial, MaterialExtension, MaterialPlugin},
    prelude::*,
    shader::{ShaderRef, load_shader_library},
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
pub(crate) struct LandNoiseImage(Handle<Image>);

impl LandNoiseImage {
    pub(crate) fn loaded(&self, server: &AssetServer) -> Option<&Handle<Image>> {
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

pub(crate) struct TerrainBlendShaderPlugin;

impl Plugin for TerrainBlendShaderPlugin {
    fn build(&self, app: &mut App) {
        load_shader_library!(app, "shaders/terrain_blend.wgsl");
        load_shader_library!(app, "shaders/terrain_normal.wgsl");
    }
}

pub(crate) struct TerrainLodNoisePlugin;

impl Plugin for TerrainLodNoisePlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<TerrainBlendShaderPlugin>() {
            app.add_plugins(TerrainBlendShaderPlugin);
        }
        embedded_asset!(app, "shaders/terrain_lod_noise.wgsl");
        app.add_plugins(MaterialPlugin::<NoisyTerrainLodMaterial>::default())
            .add_systems(Startup, load_land_noise)
            .add_systems(Update, report_land_noise_load_failure);
    }
}

fn load_land_noise(mut commands: Commands, config: Res<EngineConfig>, server: Res<AssetServer>) {
    if config.visual_profile != VisualProfile::Skyrim {
        return;
    }
    // Skyrim's shared terrain noise texture, with the pack's KTX2 extension.
    // https://github.com/Mudcrab-Team/skyrim_decomp/pull/946, G02.
    let image = server
        .load_builder()
        .with_settings(|settings: &mut ImageLoaderSettings| {
            settings.is_srgb = false;
            // stand-in(#939): The slot 15 address, filter, and mip rules are unverified.
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

fn report_land_noise_load_failure(
    image: Option<Res<LandNoiseImage>>,
    server: Res<AssetServer>,
    mut reported: Local<bool>,
) {
    if *reported {
        return;
    }
    let Some(image) = image else {
        return;
    };
    if let Some(LoadState::Failed(error)) = server.get_load_state(image.0.id()) {
        error!("Terrain LOD land noise failed to load; using the base colour: {error}");
        *reported = true;
    }
}

#[cfg(test)]
mod tests {
    use wgpu::naga::{Expression, Literal, front::wgsl};

    // Evaluate the production functions with Naga. This tests the shader's coefficients
    // and arithmetic on machines without a graphics device.
    fn evaluate(name: &str, inputs: &str, component: &str) -> f64 {
        let shader = include_str!("shaders/terrain_blend.wgsl");
        let (_, function) = shader
            .split_once(&format!("fn {name}("))
            .expect("production shader function");
        let (_, body) = function.split_once('{').expect("function body");
        let (body, _) = body.split_once('}').expect("function end");
        let (equations, result) = body.split_once("return ").expect("function result");
        let equations = equations.replace("let ", "const ");
        let result = result.trim().trim_end_matches(';');
        let module = wgsl::parse_str(&format!(
            "{inputs} {equations} const result = ({result}){component};"
        ))
        .expect("constant evaluation of terrain blend");
        let (_, result) = module
            .constants
            .iter()
            .find(|(_, item)| item.name.as_deref() == Some("result"))
            .expect("result constant");
        match module.global_expressions[result.init] {
            Expression::Literal(Literal::F32(value)) => f64::from(value),
            ref other => panic!("expected a folded f32, got {other:?}"),
        }
    }

    #[test]
    fn terrain_blend_reaches_the_reviewed_ramp_endpoints() {
        for (distance, expected) in [
            (6963.19970703125, 0.0),
            (8294.3996, 0.5),
            (9625.599609375, 1.0),
        ] {
            let value = evaluate(
                "terrain_blend_ramp",
                &format!("const distance: f32 = {distance};"),
                "",
            );
            assert!((value - expected).abs() < 0.000002);
        }
    }

    #[test]
    fn terrain_blend_gate_disables_the_full_ramp() {
        for (gate, expected) in [(0, 0.0), (1, 0.75)] {
            let value = evaluate(
                "terrain_blend_weight",
                &format!("const ramp: f32 = 0.75; const gate: f32 = {gate}.0;"),
                "",
            );
            assert!((value - expected).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn terrain_atlas_uv_scales_and_adds_the_cell_offset_before_flipping_v() {
        let inputs = "
            const cell_uv = vec2<f32>(0.5, 0.25);
            const atlas = vec4<f32>(1.0 / 4.0, 2.0 / 4.0, 1.0 / 4.0, 1.0);
        ";
        for (component, expected) in [(".x", 0.375), (".y", 0.4375)] {
            let value = evaluate("terrain_atlas_uv", inputs, component);
            assert!((value - expected).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn both_near_mesh_paths_and_lod_use_the_same_noise_sample() {
        for shader in [
            include_str!("shaders/terrain_lod_noise.wgsl"),
            include_str!("shaders/terrain.wgsl"),
            include_str!("terrain_merge/terrain_merged.wgsl"),
        ] {
            assert!(shader.starts_with("#import engine::terrain_blend::"));
            assert!(shader.contains("noisy_land_colour("));
            assert!(!shader.contains("fn noise_multiplier"));
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
            let value = evaluate(
                "noise_multiplier",
                &format!("const base = vec3<f32>({base}); const red: f32 = {red};"),
                "",
            );
            assert!((value - expected).abs() < 0.000002);
        }
    }
}
