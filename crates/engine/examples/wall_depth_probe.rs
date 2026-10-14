//! Compare the real wall/decal composite with its decal coverage at identical camera poses.
//! --assets DIR --output DIR [--legacy-depth] [--omit-decals]
//! Source geometry, hierarchy, UVs and texture alpha are retained. Only RGB/lighting are diagnostic.
use bevy::{
    camera::RenderTarget,
    core_pipeline::{
        prepass::DepthPrepass,
        tonemapping::{DebandDither, Tonemapping},
    },
    gltf::{GltfAssetLabel, GltfMaterialName},
    mesh::{MeshVertexBufferLayoutRef, VertexAttributeValues},
    pbr::{ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline},
    prelude::*,
    render::{
        RenderPlugin,
        occlusion_culling::OcclusionCulling,
        render_resource::{AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError},
        settings::{RenderCreation, WgpuSettings, WgpuSettingsPriority},
        view::screenshot::{Screenshot, ScreenshotCaptured},
    },
    shader::ShaderRef,
    window::{ExitCondition, WindowPlugin},
};
use engine::{
    color_pipeline::SceneColorPipeline,
    nif_depth::{NifDepthMaterial, NifDepthState},
    nif_material::NifMaterialPlugin,
    prepass_vertex_alpha::PrepassVertexAlphaPlugin,
};
use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, Instant},
};

const VIEWS: [(f32, f32, f32); 12] = [
    (130.0, 0.0, 0.0),
    (250.0, 0.0, 0.0),
    (500.0, 0.0, 0.0),
    (1000.0, 0.0, 0.0),
    (250.0, 30.0, 0.0),
    (250.0, -30.0, 0.0),
    (250.0, 60.0, 0.0),
    (250.0, -60.0, 0.0),
    (250.0, 80.0, 0.0),
    (250.0, -80.0, 0.0),
    (250.0, 0.0, -0.125),
    (250.0, 0.0, 0.125),
];
const WIDTH: u32 = 960;
const HEIGHT: u32 = 640;

const ID_SHADER: Handle<Shader> = bevy::asset::uuid_handle!("4958b87a-382e-44df-b86b-95654fa8bb36");
type IdMaterial = ExtendedMaterial<StandardMaterial, IdExtension>;

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
#[bind_group_data(NifDepthState)]
struct IdExtension {
    state: NifDepthState,
}
impl From<&IdExtension> for NifDepthState {
    fn from(value: &IdExtension) -> Self {
        value.state
    }
}
impl MaterialExtension for IdExtension {
    fn fragment_shader() -> ShaderRef {
        ID_SHADER.into()
    }
    fn alpha_mode() -> Option<AlphaMode> {
        Some(AlphaMode::Blend)
    }
    fn enable_prepass() -> bool {
        false
    }
    fn enable_shadows() -> bool {
        false
    }
    fn specialize(
        _: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _: &MeshVertexBufferLayoutRef,
        key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            key.bind_group_data.apply(depth);
        }
        if key.bind_group_data.alpha_mask {
            descriptor
                .fragment
                .as_mut()
                .unwrap()
                .shader_defs
                .push("MAY_DISCARD".into());
        }
        Ok(())
    }
}

enum OriginalMaterial {
    Standard(Handle<StandardMaterial>),
    Depth(Handle<NifDepthMaterial>),
}
struct Surface {
    entity: Entity,
    name: String,
    mesh: Handle<Mesh>,
    original: OriginalMaterial,
    id: Handle<IdMaterial>,
    colors: Vec<[f32; 4]>,
    id_colors: Vec<[f32; 4]>,
}

#[derive(Resource)]
struct Probe {
    output: PathBuf,
    legacy: bool,
    omit_decals: bool,
    scene: Handle<WorldAsset>,
    target: Handle<Image>,
    start: Instant,
    spawned: bool,
    prepared: bool,
    view: usize,
    phase: usize,
    frames: u32,
    pending: bool,
    center: Vec3,
    normal: Vec3,
    tangent: Vec3,
    reference: Vec<u8>,
    receiver_ids: Vec<u32>,
    decal_ids: Vec<u32>,
    surfaces: Vec<Surface>,
    results: Vec<serde_json::Value>,
}

fn main() {
    let mut assets = PathBuf::new();
    let mut output = PathBuf::from("wall-depth-probe");
    let model = "dungeons/imperial/exterior/impextwalldivider01.glb";
    let mut legacy = false;
    let mut omit_decals = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--assets" => assets = args.next().expect("asset root").into(),
            "--output" => output = args.next().expect("output directory").into(),
            "--legacy-depth" => legacy = true,
            "--omit-decals" => omit_decals = true,
            _ => panic!("unknown option {arg}"),
        }
    }
    assert!(assets.is_dir(), "--assets requires converted assets");
    std::fs::create_dir_all(&output).unwrap();
    let output = output.canonicalize().unwrap();
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(AssetPlugin {
                file_path: assets.to_string_lossy().into(),
                ..default()
            })
            .set(RenderPlugin {
                render_creation: RenderCreation::Automatic(Box::new(WgpuSettings {
                    priority: WgpuSettingsPriority::Functionality,
                    ..default()
                })),
                ..default()
            })
            .set(WindowPlugin {
                primary_window: None,
                exit_condition: ExitCondition::DontExit,
                ..default()
            })
            .disable::<bevy::winit::WinitPlugin>()
            .disable::<bevy::audio::AudioPlugin>(),
    )
    .add_plugins((
        NifMaterialPlugin,
        PrepassVertexAlphaPlugin,
        MaterialPlugin::<IdMaterial>::default(),
    ))
    .add_plugins(bevy::app::ScheduleRunnerPlugin::run_loop(
        Duration::from_millis(16),
    ));
    app.world_mut()
        .resource_mut::<Assets<Shader>>()
        .insert(
            ID_SHADER.id(),
            Shader::from_wgsl(include_str!("wall_surface_id.wgsl"), "wall_surface_id.wgsl"),
        )
        .unwrap();
    let scene = app
        .world()
        .resource::<AssetServer>()
        .load(GltfAssetLabel::Scene(0).from_asset(format!("meshes/{model}")));
    let target = app
        .world_mut()
        .resource_mut::<Assets<Image>>()
        .add(Image::new_target_texture(
            WIDTH,
            HEIGHT,
            bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
            None,
        ));
    let result = app
        .insert_resource(Probe {
            output,
            legacy,
            omit_decals,
            scene,
            target,
            start: Instant::now(),
            spawned: false,
            prepared: false,
            view: 0,
            phase: 0,
            frames: 0,
            pending: false,
            center: Vec3::ZERO,
            normal: Vec3::Z,
            tangent: Vec3::X,
            reference: vec![],
            receiver_ids: vec![],
            decal_ids: vec![],
            surfaces: vec![],
            results: vec![],
        })
        .add_systems(Update, capture)
        .run();
    if !result.is_success() {
        std::process::exit(1);
    }
}

fn diagnostic(base: &mut StandardMaterial, color: Color) {
    base.base_color = color;
    base.unlit = true;
    base.emissive = LinearRgba::BLACK;
}

type WallSurfaces<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static GltfMaterialName,
        &'static Mesh3d,
        &'static GlobalTransform,
        &'static mut Visibility,
        Option<&'static MeshMaterial3d<StandardMaterial>>,
        Option<&'static MeshMaterial3d<NifDepthMaterial>>,
    ),
>;

// Bevy supplies each independent resource and query as a system parameter.
#[allow(clippy::too_many_arguments)]
fn capture(
    mut commands: Commands,
    mut probe: ResMut<Probe>,
    server: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut standard: ResMut<Assets<StandardMaterial>>,
    mut depth: ResMut<Assets<NifDepthMaterial>>,
    mut ids: ResMut<Assets<IdMaterial>>,
    mut surfaces: WallSurfaces,
    mut cameras: Query<(&mut Transform, &mut Tonemapping), With<Camera3d>>,
    mut exit: MessageWriter<AppExit>,
) {
    if probe.start.elapsed() > Duration::from_secs(90) {
        eprintln!(
            "wall probe timed out: {:?}",
            server.get_load_states(probe.scene.id())
        );
        exit.write(AppExit::error());
        return;
    }
    if !probe.spawned {
        if server
            .get_load_states(probe.scene.id())
            .is_some_and(|(s, _, dependencies)| {
                matches!(s, bevy::asset::LoadState::Failed(_))
                    || matches!(
                        dependencies,
                        bevy::asset::RecursiveDependencyLoadState::Failed(_)
                    )
            })
        {
            exit.write(AppExit::error());
            return;
        }
        if !server.is_loaded_with_dependencies(probe.scene.id()) {
            return;
        }
        // Same placement and rebased coordinates as REFR 0001B0EE at Fort Sungard.
        commands.spawn((
            WorldAssetRoot(probe.scene.clone()),
            Transform::from_translation(Vec3::new(2443.1328, -43.77278, -3830.6895)).with_rotation(
                Quat::from_array(shared::coordinates::creation_euler_to_runtime_quaternion([
                    0.0, 0.0, 0.20000502,
                ])),
            ),
        ));
        commands.spawn((
            Camera3d::default(),
            RenderTarget::Image(probe.target.clone().into()),
            SceneColorPipeline::default(),
            DepthPrepass,
            OcclusionCulling,
            Msaa::Off,
            DebandDither::Disabled,
            Camera {
                clear_color: ClearColorConfig::Custom(Color::BLACK),
                ..default()
            },
            Transform::default(),
        ));
        probe.spawned = true;
        return;
    }
    if probe.pending {
        return;
    }
    if !probe.prepared {
        if surfaces.iter().count() != 3 {
            return;
        }
        // Give coincident receiver/decal triangles the same ID. Independent vertices
        // carry a constant ID per triangle while retaining every original alpha value.
        let mut entries: Vec<_> = surfaces
            .iter_mut()
            .map(|(entity, name, mesh, global, _, std, ext)| {
                (
                    entity,
                    name.0.clone(),
                    mesh.0.clone(),
                    *global,
                    std.map(|m| m.0.clone()),
                    ext.map(|m| m.0.clone()),
                )
            })
            .collect();
        entries.sort_by_key(|(_, name, _, _, _, _)| name.ends_with(":16"));
        let mut triangle_ids = HashMap::new();
        for (entity, name, mesh_handle, global, std, ext) in entries {
            let decal = name.ends_with(":16");
            let color = if decal {
                Color::linear_rgb(0.0, 1.0, 0.0)
            } else if name.ends_with(":12") {
                Color::linear_rgb(1.0, 0.0, 0.0)
            } else {
                Color::linear_rgb(0.0, 0.0, 1.0)
            };
            let (mut base, state, is_depth) = if let Some(handle) = ext {
                let material = depth.get(&handle).unwrap();
                (
                    material.base.clone(),
                    material.extension.state,
                    !probe.legacy,
                )
            } else {
                (
                    standard.get(&std.unwrap()).unwrap().clone(),
                    NifDepthState {
                        depth_test: true,
                        depth_write: true,
                        decal: false,
                        alpha_mask: false,
                    },
                    false,
                )
            };
            diagnostic(&mut base, color);
            if probe.legacy {
                base.depth_bias = 0.0;
            }
            let mut id_base = base.clone();
            id_base.base_color = Color::WHITE;
            let id = ids.add(IdMaterial {
                base: id_base,
                extension: IdExtension { state },
            });
            let original = if is_depth {
                OriginalMaterial::Depth(depth.add(engine::nif_depth::depth_material(base, state)))
            } else {
                OriginalMaterial::Standard(standard.add(base))
            };
            let mut mesh = meshes.get(&mesh_handle).unwrap().clone();
            mesh.duplicate_vertices();
            let VertexAttributeValues::Float32x3(positions) =
                mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap()
            else {
                panic!("wall positions must be float3");
            };
            let triangles: Vec<[Vec3; 3]> = positions
                .as_chunks::<3>()
                .0
                .iter()
                .map(|p| std::array::from_fn(|i| global.transform_point(Vec3::from_array(p[i]))))
                .collect();
            if name.ends_with(":12") {
                let triangle = triangles[44];
                probe.center = triangle.into_iter().sum::<Vec3>() / 3.0;
                probe.normal = (triangle[1] - triangle[0])
                    .cross(triangle[2] - triangle[0])
                    .normalize();
                probe.tangent = Vec3::Y.cross(probe.normal).normalize();
            }
            let colors = match mesh.attribute(Mesh::ATTRIBUTE_COLOR) {
                Some(VertexAttributeValues::Float32x4(colors)) => colors.clone(),
                None => vec![[1.0; 4]; positions.len()],
                _ => panic!("wall vertex colors must be float4"),
            };
            let mut id_colors = Vec::with_capacity(colors.len());
            for (triangle_index, triangle) in triangles.into_iter().enumerate() {
                let mut key = triangle.map(|p| p.to_array().map(f32::to_bits));
                key.sort_unstable();
                let id = if decal {
                    *triangle_ids
                        .get(&key)
                        .expect("every decal must have an exact receiver")
                } else {
                    let next_id = triangle_ids.len() as u32 + 1;
                    *triangle_ids.entry(key).or_insert(next_id)
                };
                let rgb = Color::srgb_u8(id as u8, (id >> 8) as u8, (id >> 16) as u8).to_linear();
                for i in 0..3 {
                    id_colors.push([
                        rgb.red,
                        rgb.green,
                        rgb.blue,
                        colors[triangle_index * 3 + i][3],
                    ]);
                }
            }
            mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors.clone());
            let mesh = meshes.add(mesh);
            commands.entity(entity).insert(Mesh3d(mesh.clone()));
            probe.surfaces.push(Surface {
                entity,
                name,
                mesh,
                original,
                id,
                colors,
                id_colors,
            });
        }
        probe.prepared = true;
        return;
    }
    if probe.frames == 0 {
        let (distance, angle, jitter) = VIEWS[probe.view];
        let angle = angle.to_radians();
        let eye = probe.center
            + distance * (angle.cos() * probe.normal + angle.sin() * probe.tangent)
            + probe.tangent * jitter;
        for (mut camera, mut tonemapping) in &mut cameras {
            *camera = Transform::from_translation(eye).looking_at(probe.center, Vec3::Y);
            *tonemapping = if probe.phase < 2 {
                Tonemapping::None
            } else {
                Tonemapping::TonyMcMapface
            };
        }
        for surface in &probe.surfaces {
            let decal = surface.name.ends_with(":16");
            let hidden = match probe.phase {
                0 => decal,
                1 => !decal,
                2 => surface.name.ends_with(":12"),
                _ => probe.omit_decals && decal,
            };
            let mut entity = commands.entity(surface.entity);
            entity
                .insert(if hidden {
                    Visibility::Hidden
                } else {
                    Visibility::Inherited
                })
                .remove::<MeshMaterial3d<StandardMaterial>>()
                .remove::<MeshMaterial3d<NifDepthMaterial>>()
                .remove::<MeshMaterial3d<IdMaterial>>();
            if probe.phase < 2 {
                entity.insert(MeshMaterial3d(surface.id.clone()));
                meshes
                    .get_mut(&surface.mesh)
                    .unwrap()
                    .insert_attribute(Mesh::ATTRIBUTE_COLOR, surface.id_colors.clone());
            } else {
                match &surface.original {
                    OriginalMaterial::Standard(handle) => {
                        entity.insert(MeshMaterial3d(handle.clone()));
                    }
                    OriginalMaterial::Depth(handle) => {
                        entity.insert(MeshMaterial3d(handle.clone()));
                    }
                }
                meshes
                    .get_mut(&surface.mesh)
                    .unwrap()
                    .insert_attribute(Mesh::ATTRIBUTE_COLOR, surface.colors.clone());
            }
        }
    }
    probe.frames += 1;
    if probe.frames == 32 {
        probe.pending = true;
        commands
            .spawn(Screenshot::image(probe.target.clone()))
            .observe(evaluate);
    }
}

fn green(rgb: &[u8]) -> bool {
    rgb[1] > 40
        && i32::from(rgb[1]) > i32::from(rgb[0]) + 20
        && i32::from(rgb[1]) > i32::from(rgb[2]) + 20
}

fn evaluate(
    event: On<ScreenshotCaptured>,
    mut probe: ResMut<Probe>,
    adapter: Res<bevy::render::renderer::RenderAdapterInfo>,
    mut exit: MessageWriter<AppExit>,
) {
    let image = event.image.clone().try_into_dynamic().unwrap().to_rgb8();
    let name = ["receiver-ids", "decal-ids", "coverage", "composite"][probe.phase];
    image
        .save(probe.output.join(format!("view-{}-{name}.png", probe.view)))
        .unwrap();
    let actual = image.into_raw();
    if probe.phase < 2 {
        let ids = actual
            .as_chunks::<3>()
            .0
            .iter()
            .map(|rgb| u32::from(rgb[0]) | (u32::from(rgb[1]) << 8) | (u32::from(rgb[2]) << 16))
            .collect();
        if probe.phase == 0 {
            probe.receiver_ids = ids;
        } else {
            probe.decal_ids = ids;
        }
        probe.phase += 1;
    } else if probe.phase == 2 {
        probe.reference = actual;
        probe.phase += 1;
    } else {
        let mut covered = 0;
        let mut missing = 0;
        let mut red_winners = 0;
        let mut other_surface_occlusion = 0;
        let mut mask = vec![0_u8; (WIDTH * HEIGHT * 3) as usize];
        for y in 1..HEIGHT - 1 {
            for x in 1..WIDTH - 1 {
                // Erode alpha/silhouette edges so sampling boundaries do not count as fighting.
                if !(-1_i32..=1).all(|dy| {
                    (-1_i32..=1).all(|dx| {
                        let i = (((y as i32 + dy) * WIDTH as i32 + (x as i32 + dx)) * 3) as usize;
                        green(&probe.reference[i..i + 3])
                    })
                }) {
                    continue;
                }
                let pixel = (y * WIDTH + x) as usize;
                if probe.receiver_ids[pixel] == 0
                    || probe.receiver_ids[pixel] != probe.decal_ids[pixel]
                {
                    other_surface_occlusion += 1;
                    continue;
                }
                covered += 1;
                let i = pixel * 3;
                if !green(&actual[i..i + 3]) {
                    missing += 1;
                    mask[i] = 255;
                    if actual[i] > actual[i + 1] {
                        red_winners += 1;
                    }
                }
            }
        }
        // Save a diagnostic mask without changing any source image or texture.
        std::fs::write(
            probe
                .output
                .join(format!("view-{}-missing.ppm", probe.view)),
            [format!("P6\n{WIDTH} {HEIGHT}\n255\n").into_bytes(), mask].concat(),
        )
        .unwrap();
        let passed = covered > 100 && missing == 0;
        let view = probe.view;
        probe
            .results
            .push(serde_json::json!({"view":view,"distance":VIEWS[view].0,
            "angle":VIEWS[view].1,"camera_jitter":VIEWS[view].2,"covered_pixels":covered,
            "missing_decal_pixels":missing,"receiver_winners":red_winners,
            "other_surface_occlusion_pixels":other_surface_occlusion,"passed":passed}));
        println!("wall view {view}: {missing}/{covered} decal pixels missing");
        probe.phase = 0;
        probe.view += 1;
    }
    probe.frames = 0;
    probe.pending = false;
    if probe.view == VIEWS.len() {
        let passed = probe.results.iter().all(|r| r["passed"] == true);
        let report = serde_json::json!({"kind":"real-wall-depth-coverage","legacy_depth":probe.legacy,
            "omitted_decal_control":probe.omit_decals,"occlusion_culling":true,
            "adapter":adapter.name,"target":probe.center.to_array(),"normal":probe.normal.to_array(),
            "views":probe.results,"source_geometry_and_alpha_retained":true,
            "diagnostic_rgb_and_unlit":true,"triangle_identity_guard":true,"retail_parity":false,"passed":passed});
        std::fs::write(
            probe.output.join("probe.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
        exit.write(if passed {
            AppExit::Success
        } else {
            AppExit::error()
        });
    }
}
