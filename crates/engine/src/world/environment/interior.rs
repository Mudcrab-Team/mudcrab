//! Authored interior lighting selection. The resolved values are reported, not applied.
//! See docs/specs/engine/interior-environment.md for evidence and unresolved native semantics.

use super::{EnvironmentCatalog, Lighting};
use crate::streaming::ActiveSpace;
use bevy::{prelude::*, transform::TransformSystems};
use std::collections::BTreeMap;

const SHOW_SKY: u32 = 0x80;
const USE_SKY_LIGHTING: u32 = 0x100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) enum LightingSource {
    CellXcll,
    MalformedCellXcll(u32),
    TemplateData(u32),
    TemplateDalc(u32),
    MissingTemplate(Option<u32>),
    MissingCell,
}

/// Selected values remain optional: authored black/zero never means missing.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) struct InteriorLighting {
    pub cell_id: u32,
    pub xcll_size: Option<u32>,
    pub template_id: Option<u32>,
    pub inherit_flags: Option<u32>,
    pub template_only: bool,
    pub lighting: Lighting,
    pub sources: BTreeMap<&'static str, LightingSource>,
    pub missing: Vec<&'static str>,
}

impl EnvironmentCatalog {
    pub(crate) fn resolve_interior(&self, cell_id: u32) -> InteriorLighting {
        let cell = self.cell_lighting(cell_id);
        let template_id = cell.and_then(|cell| cell.lighting_template);
        let template = template_id.and_then(|id| self.lighting_template(id));
        let local = cell.map_or(Lighting::default(), |cell| cell.lighting);
        let inherited = template.map_or(Lighting::default(), |template| template.lighting);
        let inherit_flags = cell.and_then(|cell| cell.inherit_flags);
        // LTMP-only cells have no local block to override the template. Short XCLL blocks
        // without a mask keep local values. This is a compatibility policy, not a native trace.
        let template_only = cell.is_some_and(|cell| cell.xcll_size.is_none());
        let flags = inherit_flags.unwrap_or(if template_only { 0x7ff } else { 0 });
        let mut result = InteriorLighting {
            cell_id,
            xcll_size: cell.and_then(|cell| cell.xcll_size),
            template_id,
            inherit_flags,
            template_only,
            lighting: Lighting::default(),
            sources: BTreeMap::new(),
            missing: Vec::new(),
        };
        macro_rules! select {
            ($field:ident, $bit:expr, $template_value:expr, $dalc:expr) => {{
                let from_template = flags & $bit != 0;
                let value = if from_template {
                    $template_value
                } else {
                    local.$field
                };
                let source = if from_template {
                    match (template_id, template) {
                        (Some(id), Some(_)) if $dalc => LightingSource::TemplateDalc(id),
                        (Some(id), Some(_)) => LightingSource::TemplateData(id),
                        _ => LightingSource::MissingTemplate(template_id),
                    }
                } else if let Some(size) = result.xcll_size.filter(|size| *size < 72) {
                    LightingSource::MalformedCellXcll(size)
                } else if cell.is_some() {
                    LightingSource::CellXcll
                } else {
                    LightingSource::MissingCell
                };
                result.sources.insert(stringify!($field), source);
                if value.is_none() {
                    result.missing.push(stringify!($field));
                }
                result.lighting.$field = value;
            }};
            ($field:ident, $bit:expr) => {
                select!($field, $bit, inherited.$field, false)
            };
        }
        select!(ambient, 0x1);
        // LGTM DATA's embedded ambient block is unused in the serialized format. Use DALC.
        select!(
            directional_ambient,
            0x1,
            template.and_then(|t| t.directional_ambient),
            true
        );
        select!(directional, 0x2);
        select!(fog_near_color, 0x4);
        select!(fog_far_color, 0x4);
        select!(fog_near, 0x8);
        select!(fog_far, 0x10);
        select!(directional_rotation_xy, 0x20);
        select!(directional_rotation_z, 0x20);
        select!(directional_fade, 0x40);
        select!(fog_clip_distance, 0x80);
        select!(fog_power, 0x100);
        select!(fog_max, 0x200);
        select!(light_fade_begin, 0x400);
        select!(light_fade_end, 0x400);
        result
    }
}

/// One scene selection shared by diagnostics and sky. ActiveSpace owns scene identity.
#[derive(Resource, Debug, Clone, PartialEq, Default, serde::Serialize)]
pub(crate) struct SceneEnvironment {
    pub interior: Option<InteriorLighting>,
    pub cell_flags: Option<u32>,
    pub show_sky: bool,
    pub use_sky_lighting: bool,
}

impl SceneEnvironment {
    fn select(catalog: &EnvironmentCatalog, space: &ActiveSpace) -> Self {
        let Some(cell_id) = space.interior else {
            return Self {
                show_sky: true,
                use_sky_lighting: true,
                ..default()
            };
        };
        let cell_flags = catalog.cell_flags(cell_id);
        Self {
            interior: Some(catalog.resolve_interior(cell_id)),
            cell_flags,
            show_sky: cell_flags.is_some_and(|flags| flags & SHOW_SKY != 0),
            use_sky_lighting: cell_flags.is_some_and(|flags| flags & USE_SKY_LIGHTING != 0),
        }
    }
}

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct EnvironmentSet;

pub(crate) struct InteriorEnvironmentPlugin;

impl Plugin for InteriorEnvironmentPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SceneEnvironment>().add_systems(
            PostUpdate,
            select_environment
                .in_set(EnvironmentSet)
                .after(crate::door_crossing::DoorCrossingSet)
                .before(TransformSystems::Propagate),
        );
    }
}

/// Runs after queued PostUpdate door switches, before sky and extraction. It records the
/// selection only. No Bevy light is written: the mapping from authored values to the renderer
/// has no Skyrim source yet (#693 to #696).
fn select_environment(
    catalog: Res<EnvironmentCatalog>,
    space: Res<ActiveSpace>,
    mut scene: ResMut<SceneEnvironment>,
    mut selected_once: Local<bool>,
) {
    let first_selection = !std::mem::replace(&mut *selected_once, true);
    if first_selection
        || catalog.is_changed()
        || space.is_changed()
        || scene.interior.is_none() && space.interior.is_some()
    {
        let selected = SceneEnvironment::select(&catalog, &space);
        if *scene != selected {
            info!(environment = ?selected, "selected scene environment");
            *scene = selected;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::light::CascadeShadowConfig;
    use rusqlite::Connection;

    fn fixture() -> EnvironmentCatalog {
        let connection = Connection::open_in_memory().unwrap();
        converter::esm::exporter::create_tables(&connection).unwrap();
        connection.execute_batch(
            "INSERT INTO cell_environment(cell_id,data_flags) VALUES(10,1),(11,129),(12,257),(13,385);
             INSERT INTO cell_lighting(cell_id,lighting_template_id,xcll_size,inherit_flags,
                ambient_r,ambient_g,ambient_b,directional_r,directional_g,directional_b,
                directional_rotation_xy,directional_rotation_z,directional_fade)
             VALUES(10,20,92,3,200,201,202,210,211,212,90,30,0.25),
                   (11,20,92,3,200,201,202,210,211,212,90,30,0.25);
             INSERT INTO lighting_templates(id,ambient_r,ambient_g,ambient_b,
                directional_r,directional_g,directional_b,directional_rotation_xy,
                directional_rotation_z,directional_fade)
             VALUES(20,30,40,50,60,70,80,180,60,0.75);
"
        ).unwrap();
        EnvironmentCatalog::from_connection(&connection)
    }

    pub(super) fn app(catalog: EnvironmentCatalog) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(catalog)
            .init_resource::<ActiveSpace>()
            .add_plugins(InteriorEnvironmentPlugin);
        app
    }

    pub(super) fn enter(app: &mut App, cell: Option<u32>) {
        app.world_mut().resource_mut::<ActiveSpace>().interior = cell;
        app.update();
    }

    fn selected(app: &App) -> &InteriorLighting {
        app.world()
            .resource::<SceneEnvironment>()
            .interior
            .as_ref()
            .unwrap()
    }

    #[test]
    fn interior_selection_writes_no_bevy_light() {
        let exterior_rotation = Quat::from_rotation_y(0.4);
        let mut app = app(fixture());
        app.insert_resource(GlobalAmbientLight {
            color: Color::srgb(0.1, 0.2, 0.3),
            brightness: 123.0,
            ..default()
        });
        let sun = app
            .world_mut()
            .spawn((
                DirectionalLight {
                    color: Color::srgb(0.8, 0.7, 0.6),
                    illuminance: 4321.0,
                    ..default()
                },
                CascadeShadowConfig::default(),
                Transform::from_rotation(exterior_rotation),
            ))
            .id();
        // `App::update` clears the world change trackers, so detect writes with a system:
        // its change ticks survive across updates. The first two updates settle added state.
        #[derive(Resource, Default)]
        struct LightWrites(u32);
        app.init_resource::<LightWrites>().add_systems(
            PostUpdate,
            |lights: Query<(Ref<DirectionalLight>, Ref<Transform>, Ref<CascadeShadowConfig>)>,
             ambient: Res<GlobalAmbientLight>,
             mut writes: ResMut<LightWrites>| {
                for (light, transform, cascades) in &lights {
                    writes.0 += (light.is_changed() || transform.is_changed() || cascades.is_changed())
                        as u32;
                }
                writes.0 += ambient.is_changed() as u32;
            },
        );
        app.update();
        app.update();
        app.world_mut().resource_mut::<LightWrites>().0 = 0;
        // Cell 10 authors non-black ambient and directional colours, a nonzero fade and a
        // rotation, and does not use sky lighting.
        enter(&mut app, Some(10));
        let lighting = &selected(&app).lighting;
        assert_eq!(lighting.ambient, Some([30, 40, 50]));
        assert_eq!(lighting.directional, Some([60, 70, 80]));
        assert_eq!(lighting.directional_fade, Some(0.25));
        assert_eq!(lighting.directional_rotation_xy, Some(90));
        assert!(!app.world().resource::<SceneEnvironment>().use_sky_lighting);

        let entity = app.world().entity(sun);
        let light = entity.get_ref::<DirectionalLight>().unwrap();
        assert_eq!(light.color, Color::srgb(0.8, 0.7, 0.6));
        assert_eq!(light.illuminance, 4321.0);
        let transform = entity.get_ref::<Transform>().unwrap();
        assert_eq!(transform.rotation, exterior_rotation);
        let cascades = entity.get_ref::<CascadeShadowConfig>().unwrap();
        assert_eq!(
            format!("{:?}", *cascades),
            format!("{:?}", CascadeShadowConfig::default())
        );
        let ambient = app.world().resource::<GlobalAmbientLight>();
        assert_eq!(ambient.color, Color::srgb(0.1, 0.2, 0.3));
        assert_eq!(ambient.brightness, 123.0);
        assert_eq!(
            app.world().resource::<LightWrites>().0,
            0,
            "the interior selection wrote a Bevy light"
        );
    }

    #[test]
    fn local_inherited_zero_and_missing_values_are_reported_unchanged() {
        let mut catalog = fixture();
        {
            let cell = catalog.cells.get_mut(&10).unwrap();
            cell.inherit_flags = Some(0);
            cell.lighting.ambient = Some([0; 3]);
            cell.lighting.directional = Some([0; 3]);
            cell.lighting.directional_rotation_xy = Some(0);
            cell.lighting.directional_rotation_z = Some(0);
            cell.lighting.directional_fade = Some(0.0);
        }
        let mut app = app(catalog);
        enter(&mut app, Some(10));
        let lighting = &selected(&app).lighting;
        assert_eq!(lighting.ambient, Some([0; 3]));
        assert_eq!(lighting.directional, Some([0; 3]));
        assert_eq!(lighting.directional_fade, Some(0.0));
        assert!(!selected(&app).missing.contains(&"directional_fade"));
        assert_eq!(
            selected(&app).sources["directional"],
            LightingSource::CellXcll
        );
        {
            let mut catalog = app.world_mut().resource_mut::<EnvironmentCatalog>();
            let cell = catalog.cells.get_mut(&10).unwrap();
            cell.inherit_flags = Some(3);
            cell.lighting_template = Some(999);
        }
        app.update();
        assert_eq!(selected(&app).lighting.directional, None);
        assert_eq!(
            selected(&app).sources["directional"],
            LightingSource::MissingTemplate(Some(999))
        );
        assert!(selected(&app).missing.contains(&"directional"));
        enter(&mut app, Some(404));
        assert_eq!(selected(&app).lighting, Lighting::default());
        enter(&mut app, None);
        assert!(
            app.world()
                .resource::<SceneEnvironment>()
                .interior
                .is_none()
        );
    }

    #[test]
    fn template_dalc_uses_separate_subrecord_and_black_is_not_missing() {
        let mut catalog = fixture();
        let template = catalog.templates.get_mut(&20).unwrap();
        template.lighting.directional_ambient = Some(super::super::DirectionalAmbient {
            colors: [[255; 3]; 6],
            ..default()
        });
        template.directional_ambient = Some(super::super::DirectionalAmbient {
            colors: [[0; 3]; 6],
            scale: Some(25.0),
            ..default()
        });
        let mut app = app(catalog);
        enter(&mut app, Some(10));
        let cube = selected(&app).lighting.directional_ambient.unwrap();
        assert_eq!(cube.colors, [[0; 3]; 6]);
        assert_eq!(cube.scale, Some(25.0));
        assert!(!selected(&app).missing.contains(&"directional_ambient"));
        assert_eq!(
            selected(&app).sources["directional_ambient"],
            LightingSource::TemplateDalc(20)
        );
        {
            let mut catalog = app.world_mut().resource_mut::<EnvironmentCatalog>();
            let template = catalog.templates.get_mut(&20).unwrap();
            template.directional_ambient.as_mut().unwrap().colors =
                [[255, 0, 0], [0; 3], [0; 3], [0; 3], [0; 3], [0; 3]];
        }
        app.update();
        assert_eq!(
            selected(&app).lighting.directional_ambient.unwrap().colors,
            [[255, 0, 0], [0; 3], [0; 3], [0; 3], [0; 3], [0; 3]]
        );
    }

    #[test]
    fn every_inheritance_group_and_absent_mask_policy_is_explicit() {
        let mut catalog = fixture();
        let local = Lighting {
            ambient: Some([1; 3]),
            directional: Some([2; 3]),
            fog_near_color: Some([3; 3]),
            fog_far_color: Some([4; 3]),
            fog_near: Some(5.0),
            fog_far: Some(6.0),
            directional_rotation_xy: Some(7),
            directional_rotation_z: Some(8),
            directional_fade: Some(9.0),
            fog_clip_distance: Some(10.0),
            fog_power: Some(11.0),
            fog_max: Some(12.0),
            light_fade_begin: Some(13.0),
            light_fade_end: Some(14.0),
            directional_ambient: None,
        };
        catalog.cells.get_mut(&10).unwrap().lighting = local;
        catalog.cells.get_mut(&10).unwrap().inherit_flags = Some(0);
        assert_eq!(catalog.resolve_interior(10).lighting, local);
        catalog.cells.get_mut(&10).unwrap().inherit_flags = Some(0x7ff);
        let selected = catalog.resolve_interior(10);
        assert_eq!(selected.lighting, catalog.templates[&20].lighting);
        assert!(selected.sources.values().all(|source| matches!(
            source,
            LightingSource::TemplateData(20) | LightingSource::TemplateDalc(20)
        )));
        // Missing template fields remain missing, rather than borrowing a local value.
        assert_eq!(selected.lighting.fog_near, None);
        assert!(selected.missing.contains(&"fog_near"));
        let cell = catalog.cells.get_mut(&10).unwrap();
        cell.xcll_size = Some(72);
        cell.inherit_flags = None;
        assert_eq!(catalog.resolve_interior(10).lighting, local);
        let cell = catalog.cells.get_mut(&10).unwrap();
        cell.xcll_size = None;
        cell.lighting = Lighting::default();
        assert!(catalog.resolve_interior(10).template_only);
        assert_eq!(
            catalog.resolve_interior(10).lighting.directional,
            Some([60, 70, 80])
        );
    }

    #[test]
    fn individual_inheritance_bits_select_independent_scene_fields() {
        let mut catalog = fixture();
        catalog
            .cells
            .get_mut(&10)
            .unwrap()
            .lighting
            .directional_ambient = Some(super::super::DirectionalAmbient {
            colors: [[255, 0, 0]; 6],
            ..default()
        });
        catalog.templates.get_mut(&20).unwrap().directional_ambient =
            Some(super::super::DirectionalAmbient {
                colors: [[0, 255, 0]; 6],
                ..default()
            });
        let mut app = app(catalog);
        for mask in [0, 1, 2, 0x20, 0x40, 0x61, 0x7ff, 0] {
            app.world_mut()
                .resource_mut::<EnvironmentCatalog>()
                .cells
                .get_mut(&10)
                .unwrap()
                .inherit_flags = Some(mask);
            enter(&mut app, Some(10));
            let lighting = &selected(&app).lighting;
            assert_eq!(
                lighting.directional,
                Some(if mask & 2 != 0 {
                    [60, 70, 80]
                } else {
                    [210, 211, 212]
                })
            );
            assert_eq!(
                lighting.directional_fade,
                Some(if mask & 0x40 != 0 { 0.75 } else { 0.25 })
            );
            assert_eq!(
                lighting.directional_ambient.unwrap().colors,
                if mask & 1 != 0 {
                    [[0, 255, 0]; 6]
                } else {
                    [[255, 0, 0]; 6]
                }
            );
            assert_eq!(
                (
                    lighting.directional_rotation_xy,
                    lighting.directional_rotation_z
                ),
                if mask & 0x20 != 0 {
                    (Some(180), Some(60))
                } else {
                    (Some(90), Some(30))
                }
            );
        }
    }

    #[test]
    fn show_sky_and_use_sky_lighting_are_independent_runtime_choices() {
        let mut app = app(fixture());
        for (cell, sky, sky_lighting) in [
            (10, false, false),
            (11, true, false),
            (12, false, true),
            (13, true, true),
        ] {
            enter(&mut app, Some(cell));
            let scene = app.world().resource::<SceneEnvironment>();
            assert_eq!(scene.show_sky, sky);
            assert_eq!(scene.use_sky_lighting, sky_lighting);
        }
    }

    #[test]
    fn old_database_reports_every_authored_field_missing() {
        let catalog = EnvironmentCatalog::from_connection(&Connection::open_in_memory().unwrap());
        let mut app = app(catalog);
        app.update();
        enter(&mut app, Some(10));
        let scene = app.world().resource::<SceneEnvironment>();
        assert!(!scene.show_sky);
        assert_eq!(
            scene.interior.as_ref().unwrap().sources["ambient"],
            LightingSource::MissingCell
        );
        assert_eq!(selected(&app).lighting, Lighting::default());
        enter(&mut app, None);
        let scene = app.world().resource::<SceneEnvironment>();
        assert!(scene.interior.is_none());
        assert!(scene.show_sky && scene.use_sky_lighting);
    }
}

#[cfg(test)]
mod plugin_tests {
    use super::*;

    fn sub(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
        [tag.as_slice(), &(bytes.len() as u16).to_le_bytes(), bytes].concat()
    }

    fn record(tag: &[u8; 4], id: u32, payload: &[u8]) -> Vec<u8> {
        [
            tag.as_slice(),
            &(payload.len() as u32).to_le_bytes(),
            &0u32.to_le_bytes(),
            &id.to_le_bytes(),
            &[0; 4],
            &44u16.to_le_bytes(),
            &[0; 2],
            payload,
        ]
        .concat()
    }

    fn group(kind: u32, label: u32, payload: &[u8]) -> Vec<u8> {
        [
            b"GRUP".as_slice(),
            &(24 + payload.len() as u32).to_le_bytes(),
            &label.to_le_bytes(),
            &kind.to_le_bytes(),
            &[0; 8],
            payload,
        ]
        .concat()
    }

    fn header(master: bool) -> Vec<u8> {
        let mut payload = sub(
            b"HEDR",
            &[1.7f32.to_le_bytes(), [0; 4], 0x800u32.to_le_bytes()].concat(),
        );
        if !master {
            payload.extend(sub(b"MAST", b"Base.esm\0"));
            payload.extend(sub(b"DATA", &[0; 8]));
        }
        let mut result = record(b"TES4", 0, &payload);
        result[8..12].copy_from_slice(&u32::from(master).to_le_bytes());
        result
    }

    fn selected(app: &App) -> &InteriorLighting {
        app.world()
            .resource::<SceneEnvironment>()
            .interior
            .as_ref()
            .unwrap()
    }

    #[test]
    fn procedural_plugin_override_reaches_the_scene_environment() {
        // A new cell/template pair, with an unrelated plugin between master and override.
        // Both the identity remap and the override winner go through the actual converter.
        let root = tempfile::tempdir().unwrap();
        let paths = ["Base.esm", "Other.esm", "Override.esp"].map(|name| root.path().join(name));
        let mut data = [0u8; 92];
        data[0..3].copy_from_slice(&[32, 48, 64]);
        data[4..7].copy_from_slice(&[64, 96, 128]);
        data[28..32].copy_from_slice(&0.5f32.to_le_bytes());
        let template_record = |data: &[u8]| {
            group(
                0,
                u32::from_le_bytes(*b"LGTM"),
                &record(
                    b"LGTM",
                    0x800,
                    &[sub(b"EDID", b"NewTemplate\0"), sub(b"DATA", data)].concat(),
                ),
            )
        };
        let mut cell_data = data;
        cell_data[88..92].copy_from_slice(&0x43u32.to_le_bytes());
        let cell = record(
            b"CELL",
            0x801,
            &[
                sub(b"EDID", b"NewInterior\0"),
                sub(b"DATA", &129u16.to_le_bytes()),
                sub(b"LTMP", &0x800u32.to_le_bytes()),
                sub(b"XCLL", &cell_data),
            ]
            .concat(),
        );
        let cell = group(
            0,
            u32::from_le_bytes(*b"CELL"),
            &group(2, 0, &group(3, 0, &cell)),
        );
        std::fs::write(
            &paths[0],
            [header(true), template_record(&data), cell].concat(),
        )
        .unwrap();
        std::fs::write(&paths[1], header(true)).unwrap();
        for (index, color) in [[64, 96, 128], [0, 0, 0], [255, 32, 16]]
            .into_iter()
            .enumerate()
        {
            data[4..7].copy_from_slice(&color);
            // This plugin has one master, but loads at index 2. Its override references index 0.
            std::fs::write(&paths[2], [header(false), template_record(&data)].concat()).unwrap();
            let database = root.path().join(format!("case-{index}.db"));
            converter::esm::EsmParser::convert_plugins(&paths, &database).unwrap();
            let catalog = EnvironmentCatalog::open(&database).unwrap();
            let resolved = catalog.resolve_interior(0x801);
            assert_eq!(resolved.lighting.directional, Some(color));
            assert_eq!(
                resolved.sources["directional"],
                LightingSource::TemplateData(0x800)
            );
            let mut app = super::tests::app(catalog);
            super::tests::enter(&mut app, Some(0x801));
            assert_eq!(selected(&app).lighting.directional, Some(color));
            // The override changes the directional colour only. Authored ambient is unchanged.
            assert_eq!(selected(&app).lighting.ambient, Some([32, 48, 64]));
            assert_eq!(selected(&app).lighting.directional_fade, Some(0.5));
            assert!(app.world().resource::<SceneEnvironment>().show_sky);
            assert!(!app.world().resource::<SceneEnvironment>().use_sky_lighting);
        }
    }

    #[test]
    fn absent_and_malformed_xcll_do_not_select_the_same_template_lighting() {
        use converter::config::RecordReader;

        for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("Base.esm");
            let mut data = [0u8; 92];
            data[0..3].copy_from_slice(&[255, 0, 0]);
            data[4..7].copy_from_slice(&[64, 96, 128]);
            data[28..32].copy_from_slice(&0.5f32.to_le_bytes());
            for face in data[40..64].as_chunks_mut::<4>().0 {
                face[..3].copy_from_slice(&[255, 0, 0]);
            }
            let template = group(
                0,
                u32::from_le_bytes(*b"LGTM"),
                &record(b"LGTM", 0x800, &sub(b"DATA", &data)),
            );
            let mut cells = Vec::new();
            for (index, size) in [None, Some(0), Some(71), Some(72), Some(92)]
                .into_iter()
                .enumerate()
            {
                let mut payload = [
                    sub(b"DATA", &1u16.to_le_bytes()),
                    sub(b"LTMP", &0x800u32.to_le_bytes()),
                ]
                .concat();
                if let Some(size) = size {
                    payload.extend(sub(b"XCLL", &data[..size]));
                }
                cells.extend(record(b"CELL", 0x801 + index as u32, &payload));
            }
            cells.extend(record(
                b"CELL",
                0x806,
                &[sub(b"DATA", &[1, 0]), sub(b"XCLL", &data[..71])].concat(),
            ));
            let cells = group(
                0,
                u32::from_le_bytes(*b"CELL"),
                &group(2, 0, &group(3, 0, &cells)),
            );
            std::fs::write(&path, [header(true), template, cells].concat()).unwrap();
            let database = root.path().join("world.db");
            converter::esm::EsmParser::convert_plugins_with_reader(
                &[path],
                &database,
                reader,
                root.path(),
            )
            .unwrap();
            let catalog = EnvironmentCatalog::open(&database).unwrap();
            for (index, size) in [None, Some(0), Some(71), Some(72), Some(92)]
                .into_iter()
                .enumerate()
            {
                let id = 0x801 + index as u32;
                assert_eq!(catalog.cell_lighting(id).unwrap().xcll_size, size);
                assert_eq!(catalog.resolve_interior(id).template_only, size.is_none());
            }
            assert_eq!(catalog.cell_lighting(0x806).unwrap().xcll_size, Some(71));
            let mut app = super::tests::app(catalog);
            for cell in [0x801, 0x802, 0x803, 0x804, 0x805, 0x806, 0x801] {
                super::tests::enter(&mut app, Some(cell));
                let resolved = selected(&app);
                let malformed = matches!(cell, 0x802 | 0x803 | 0x806);
                assert_eq!(
                    resolved.sources["directional"],
                    if cell == 0x801 {
                        LightingSource::TemplateData(0x800)
                    } else if malformed {
                        LightingSource::MalformedCellXcll(if cell == 0x802 { 0 } else { 71 })
                    } else {
                        LightingSource::CellXcll
                    }
                );
                assert_eq!(resolved.missing.contains(&"directional"), malformed);
                assert_eq!(
                    resolved.lighting.directional,
                    (!malformed).then_some([64, 96, 128])
                );
                assert_eq!(
                    resolved.lighting.ambient,
                    (!malformed).then_some([255, 0, 0])
                );
            }
        }
    }

    #[test]
    fn procedural_dalc_overrides_reach_the_scene_environment_unchanged() {
        use converter::config::RecordReader;

        for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
            let root = tempfile::tempdir().unwrap();
            let paths = ["Base.esm", "Override.esp"].map(|name| root.path().join(name));
            let mut data = [0u8; 92];
            data[0..3].copy_from_slice(&[255; 3]);
            data[4..7].copy_from_slice(&[40, 50, 60]);
            data[28..32].copy_from_slice(&0.5f32.to_le_bytes());
            let template = |cube: &[u8]| {
                group(
                    0,
                    u32::from_le_bytes(*b"LGTM"),
                    &record(
                        b"LGTM",
                        0x800,
                        &[sub(b"DATA", &data), sub(b"DALC", cube)].concat(),
                    ),
                )
            };
            let mut local = data;
            local[88..92].copy_from_slice(&0x43u32.to_le_bytes());
            let cell = record(
                b"CELL",
                0x801,
                &[
                    sub(b"DATA", &[1, 0]),
                    sub(b"LTMP", &0x800u32.to_le_bytes()),
                    sub(b"XCLL", &local),
                ]
                .concat(),
            );
            let cells = group(
                0,
                u32::from_le_bytes(*b"CELL"),
                &group(2, 0, &group(3, 0, &cell)),
            );
            std::fs::write(
                &paths[0],
                [header(true), template(&[0; 24]), cells].concat(),
            )
            .unwrap();
            let mut app = super::tests::app(EnvironmentCatalog::default());
            for (index, face) in [[0, 255, 0], [0, 0, 0], [255, 0, 0]]
                .into_iter()
                .enumerate()
            {
                let cube: Vec<_> = (0..6)
                    .flat_map(|_| [face[0], face[1], face[2], 0])
                    .collect();
                std::fs::write(&paths[1], [header(false), template(&cube)].concat()).unwrap();
                let database = root.path().join(format!("{index}.db"));
                converter::esm::EsmParser::convert_plugins_with_reader(
                    &paths,
                    &database,
                    reader,
                    root.path(),
                )
                .unwrap();
                app.insert_resource(EnvironmentCatalog::open(&database).unwrap());
                super::tests::enter(&mut app, Some(0x801));
                let resolved = selected(&app);
                assert_eq!(
                    resolved.sources["directional_ambient"],
                    LightingSource::TemplateDalc(0x800)
                );
                assert_eq!(
                    resolved.lighting.directional_ambient.unwrap().colors,
                    [face; 6]
                );
                assert_eq!(resolved.lighting.directional, Some([40, 50, 60]));
            }
        }
    }

    #[test]
    #[ignore = "opt-in read-only managed-pack check; set MUDCRAB_INTERIOR_TEST_DB"]
    fn managed_farm_cells_reach_the_scene_environment_without_modifying_pack() {
        let path =
            std::env::var_os("MUDCRAB_INTERIOR_TEST_DB").expect("explicit managed database path");
        let catalog = EnvironmentCatalog::open(std::path::Path::new(&path)).unwrap();
        let mut app = super::tests::app(catalog);
        for (cell, elevation) in [(78791, 0), (78792, 30)] {
            super::tests::enter(&mut app, Some(cell));
            let scene = app.world().resource::<SceneEnvironment>();
            let resolved = scene.interior.as_ref().unwrap();
            assert_eq!(resolved.template_id, Some(659862));
            assert_eq!(resolved.lighting.directional, Some([65, 70, 80]));
            assert_eq!(resolved.lighting.directional_rotation_z, Some(elevation));
            assert_eq!(resolved.lighting.directional_fade, Some(0.0));
            assert!(scene.show_sky);
            assert!(!scene.use_sky_lighting);
        }
    }
}
