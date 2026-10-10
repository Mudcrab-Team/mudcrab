//! Skyrim's `CenterOnExterior` (`coe`) and `CenterOnCell` (`coc`) console commands, exterior
//! cells only.
//!
//! Both read the world database for the target cell, take its ground height from the cell cache, and
//! send a [`TeleportPlayer`]; [`crate::physics`] places the player and the camera, and the
//! streaming plugin loads the new area around the camera (re-centring its render origin as
//! needed). Skyrim puts the player on the cell's COC marker; the converter does not export those
//! yet, so the player lands on the cell centre instead.

use bevy::prelude::*;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use crate::{
    config::EngineConfig,
    console::{AppConsoleExt, ConsoleCommand},
    door_crossing::DoorCrossing,
    physics::{LookIntent, MovementTuning, TeleportPlayer, body_and_camera_for_feet},
    sky::CameraSpace,
    streaming::{ActiveSpace, RenderOrigin, StreamingWorld, unload_all_cells_now},
    world::{cache::CellCache, components::CELL_SIZE},
};

/// Add `CenterOnExterior` and `CenterOnCell` to the console registry.
pub fn register_cell_commands(app: &mut App) {
    app.add_console_command(center_on_exterior_command())
        .add_console_command(center_on_cell_command());
}

fn center_on_exterior_command() -> ConsoleCommand {
    ConsoleCommand {
        name: "CenterOnExterior",
        short: Some("coe"),
        usage: "CenterOnExterior <x> <y>",
        help: "move the player to exterior cell x y of the current worldspace",
        handler: Box::new(|world, args| {
            let (grid_x, grid_y) = parse_grid_args(args)?;
            let (worldspace_id, connection) = open_world(world)?;
            let cell_id = find_exterior_cell(&connection, worldspace_id, grid_x, grid_y)?
                .ok_or_else(|| format!("no exterior cell at {grid_x} {grid_y}"))?;
            teleport_to_cell(world, worldspace_id, cell_id, grid_x, grid_y)?;
            Ok(format!("moved to exterior cell {grid_x} {grid_y}"))
        }),
    }
}

fn center_on_cell_command() -> ConsoleCommand {
    ConsoleCommand {
        name: "CenterOnCell",
        short: Some("coc"),
        usage: "CenterOnCell <cell editor id>",
        help: "move the player to an exterior cell by editor id (interiors: not yet)",
        handler: Box::new(|world, args| {
            let name = parse_cell_name(args)?;
            let (worldspace_id, connection) = open_world(world)?;
            let cell = find_cell_by_editor_id(&connection, name, worldspace_id)?
                .ok_or_else(|| format!("no cell named {name}"))?;
            let (grid_x, grid_y) = exterior_target(&cell, worldspace_id, name)?;
            // Read the height from the cell streaming would load at that square, which is the
            // named cell unless the database holds two cells for it.
            let cell_id =
                find_exterior_cell(&connection, worldspace_id, grid_x, grid_y)?.unwrap_or(cell.id);
            teleport_to_cell(world, worldspace_id, cell_id, grid_x, grid_y)?;
            Ok(format!("moved to {name} (exterior cell {grid_x} {grid_y})"))
        }),
    }
}

/// `coe` takes exactly two integer grid coordinates.
pub fn parse_grid_args(args: &[&str]) -> Result<(i32, i32), String> {
    const USAGE: &str = "usage: CenterOnExterior <x> <y>";
    let [x, y] = args else {
        return Err(USAGE.to_owned());
    };
    match (x.parse::<i32>(), y.parse::<i32>()) {
        (Ok(x), Ok(y)) => Ok((x, y)),
        _ => Err(format!("{USAGE} (x and y are whole numbers)")),
    }
}

/// `coc` takes exactly one cell editor id.
pub fn parse_cell_name<'a>(args: &[&'a str]) -> Result<&'a str, String> {
    match args {
        [name] if !name.is_empty() => Ok(name),
        _ => Err("usage: CenterOnCell <cell editor id>".to_owned()),
    }
}

/// A `cells` row: interiors have no worldspace and no grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellRow {
    pub id: u32,
    pub worldspace_id: Option<u32>,
    pub grid: Option<(i32, i32)>,
}

/// The cell with this editor id, ignoring case. `cells.interior_name` holds the editor id of
/// interiors and of named exteriors alike, and is not unique: when two cells share a name, a
/// cell with a grid position in `streaming_worldspace` wins, then one in that worldspace without
/// a grid, then the lowest id, so the answer is stable.
pub fn find_cell_by_editor_id(
    connection: &Connection,
    name: &str,
    streaming_worldspace: u32,
) -> Result<Option<CellRow>, String> {
    connection
        .query_row(
            "SELECT id, worldspace_id, grid_x, grid_y FROM cells \
             WHERE interior_name = ?1 COLLATE NOCASE \
             ORDER BY (worldspace_id IS ?2 AND grid_x IS NOT NULL AND grid_y IS NOT NULL) DESC, \
                      worldspace_id IS ?2 DESC, id LIMIT 1",
            params![name, streaming_worldspace],
            |row| {
                let grid_x: Option<i32> = row.get(2)?;
                let grid_y: Option<i32> = row.get(3)?;
                Ok(CellRow {
                    id: row.get(0)?,
                    worldspace_id: row.get(1)?,
                    grid: grid_x.zip(grid_y),
                })
            },
        )
        .optional()
        .map_err(|e| format!("world database query failed: {e}"))
}

/// The exterior cell at this grid square of this worldspace, using the streaming plugin's query.
pub fn find_exterior_cell(
    connection: &Connection,
    worldspace_id: u32,
    grid_x: i32,
    grid_y: i32,
) -> Result<Option<u32>, String> {
    connection
        .query_row(
            crate::world::database::EXTERIOR_CELL_ID_SQL,
            params![worldspace_id, grid_x, grid_y],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("world database query failed: {e}"))
}

/// The grid square `coc` moves to, or why it cannot.
pub fn exterior_target(
    cell: &CellRow,
    streaming_worldspace: u32,
    name: &str,
) -> Result<(i32, i32), String> {
    let Some(worldspace_id) = cell.worldspace_id else {
        return Err(format!(
            "{name} is an interior cell; interior cells are not supported yet"
        ));
    };
    let Some(grid) = cell.grid else {
        return Err(format!("{name} is an exterior cell with no grid position"));
    };
    if worldspace_id != streaming_worldspace {
        return Err(format!(
            "{name} is in worldspace {worldspace_id:08X}, but this run streams {streaming_worldspace:08X}"
        ));
    }
    Ok(grid)
}

fn open_world(world: &World) -> Result<(u32, Connection), String> {
    if world
        .get_resource::<DoorCrossing>()
        .is_some_and(DoorCrossing::is_active)
    {
        return Err("wait for the door crossing to finish before teleporting".to_owned());
    }
    let config = world
        .get_resource::<EngineConfig>()
        .ok_or("no world is loaded in this run")?;
    let path = config.assets_dir.join("skyrim_world.db");
    let connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let worldspace_id = world
        .get_resource::<ActiveSpace>()
        .map_or(config.worldspace_id, |space| {
            space.exterior_worldspace(config.worldspace_id)
        });
    Ok((worldspace_id, connection))
}

/// Render-space feet position for the centre of grid square `grid` at `ground`.
pub fn cell_centre_feet(grid: (i32, i32), origin: IVec2, ground: f32) -> Vec3 {
    let dx = i64::from(grid.0) - i64::from(origin.x);
    let dy = i64::from(grid.1) - i64::from(origin.y);
    Vec3::new(
        (dx as f32 + 0.5) * CELL_SIZE,
        ground,
        -(dy as f32 + 0.5) * CELL_SIZE,
    )
}

/// How far above the terrain sample the feet land, the same clearance the start position uses,
/// so a walking capsule does not start inside the ground. The destination's static collision is
/// not loaded yet when the teleport runs, so there is nothing to search for a free spot against.
pub const LANDING_CLEARANCE: f32 = 16.0;

/// The teleport itself, kept apart so it can be swapped for another way of placing the player.
/// A cell with no terrain in the cache is refused rather than guessed at: worldspaces sit
/// thousands of units above or below 0, so a made-up height can drop the player under the world.
fn teleport_to_cell(
    world: &mut World,
    worldspace_id: u32,
    cell_id: u32,
    grid_x: i32,
    grid_y: i32,
) -> Result<(), String> {
    let origin = world
        .get_resource::<RenderOrigin>()
        .ok_or("no world is streaming in this run")?
        .0;
    let ground = world
        .get_resource::<CellCache>()
        .and_then(|cache| cache.centre_height(cell_id))
        .ok_or_else(|| format!("cell {grid_x} {grid_y} has no terrain height to stand on"))?;
    let yaw = world
        .get_resource::<LookIntent>()
        .map_or(0.0, |look| look.yaw);
    if !world.contains_resource::<Messages<TeleportPlayer>>() {
        return Err("no player to move in this run".to_owned());
    }
    let tuning = world
        .get_resource::<MovementTuning>()
        .ok_or("no player to move in this run")?;
    let position = cell_centre_feet((grid_x, grid_y), origin, ground + LANDING_CLEARANCE);
    let (_, eye) = body_and_camera_for_feet(tuning, position);
    let leaving_interior = world
        .get_resource::<ActiveSpace>()
        .is_some_and(|space| space.interior.is_some());
    if leaving_interior {
        if !world.contains_resource::<StreamingWorld>() {
            return Err("no world is streaming in this run".to_owned());
        }
        unload_all_cells_now(world);
    }
    world.insert_resource(ActiveSpace {
        worldspace_id: Some(worldspace_id),
        interior: None,
    });
    world.insert_resource(CameraSpace::Exterior);
    // The planner can run before the teleport reader. It must already see the exterior landing.
    let mut cameras =
        world.query_filtered::<&mut Transform, With<crate::world::components::StreamingCamera>>();
    for mut camera in cameras.iter_mut(world) {
        camera.translation = eye;
    }
    world.write_message(TeleportPlayer { position, yaw });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        console::{ConsoleState, execute_line},
        physics::headless,
        profiling::ProfilingState,
        streaming::{StreamingMetrics, TerrainContinuity},
        world::{components::StreamingCamera, database::CellKey},
    };

    fn database() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        let connection = Connection::open(directory.path().join("skyrim_world.db")).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE cells(id INTEGER PRIMARY KEY,worldspace_id INTEGER,grid_x INTEGER,grid_y INTEGER,interior_name TEXT);
                 -- EXTERIOR_CELL_ID_SQL LEFT JOINs `land` (rows only break ties), so it must exist.
                 CREATE TABLE land(cell_id INTEGER PRIMARY KEY);
                 INSERT INTO cells VALUES(1,60,4,-12,'Riverwood');
                 INSERT INTO cells VALUES(2,60,5,-12,NULL);
                 INSERT INTO cells VALUES(3,NULL,NULL,NULL,'WhiterunBanneredMare');
                 INSERT INTO cells VALUES(4,61,0,0,'OtherWorldCell');
                 -- A second Riverwood in another worldspace, with a lower id: the streaming
                 -- worldspace's cell must still win.
                 INSERT INTO cells VALUES(0,61,7,7,'Riverwood');
                 INSERT INTO cells VALUES(9,61,4,-12,'OtherWorldRiverwood');
                 -- An exterior with no terrain in the cell cache, and one with no grid.
                 INSERT INTO cells VALUES(5,60,6,-12,'NoLand');
                 INSERT INTO cells VALUES(6,60,NULL,NULL,'NoGrid');
                 -- A name shared by a gridless cell of the streaming worldspace (lower id) and its
                 -- real exterior: the one with a grid must win.
                 INSERT INTO cells VALUES(7,60,NULL,NULL,'SharedName');
                 INSERT INTO cells VALUES(8,60,9,9,'SharedName');",
            )
            .unwrap();
        directory
    }

    fn connection(directory: &tempfile::TempDir) -> Connection {
        Connection::open(directory.path().join("skyrim_world.db")).unwrap()
    }

    #[test]
    fn coe_arguments_need_two_whole_numbers() {
        assert_eq!(parse_grid_args(&["4", "-12"]), Ok((4, -12)));
        for bad in [
            &[][..],
            &["1"],
            &["1", "2", "3"],
            &["a", "2"],
            &["1", "2.5"],
            &[""],
        ] {
            assert!(
                parse_grid_args(bad).unwrap_err().contains("usage"),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn coc_takes_exactly_one_name() {
        assert_eq!(parse_cell_name(&["Riverwood"]), Ok("Riverwood"));
        assert!(parse_cell_name(&[]).is_err());
        for bad in [&[][..], &[""], &["a", "b"]] {
            assert!(
                parse_cell_name(bad).unwrap_err().contains("usage"),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn editor_id_lookup_ignores_case_and_reports_unknown_names() {
        let directory = database();
        let connection = connection(&directory);
        let row = find_cell_by_editor_id(&connection, "rIvErWoOd", 60)
            .unwrap()
            .unwrap();
        assert_eq!(
            row,
            CellRow {
                id: 1,
                worldspace_id: Some(60),
                grid: Some((4, -12))
            }
        );
        assert_eq!(
            find_cell_by_editor_id(&connection, "riverwood", 61)
                .unwrap()
                .map(|row| row.id),
            Some(0),
            "the other worldspace's Riverwood when that one streams"
        );
        assert_eq!(
            find_cell_by_editor_id(&connection, "Nowhere", 60).unwrap(),
            None
        );
        assert_eq!(
            find_exterior_cell(&connection, 60, 5, -12).unwrap(),
            Some(2)
        );
        assert_eq!(find_exterior_cell(&connection, 60, 99, 99).unwrap(), None);
    }

    #[test]
    fn a_shared_name_prefers_the_streaming_worldspaces_cell_with_a_grid() {
        let directory = database();
        let connection = connection(&directory);
        let row = find_cell_by_editor_id(&connection, "SharedName", 60)
            .unwrap()
            .unwrap();
        assert_eq!(
            row.id, 8,
            "the gridded exterior, not the lower-id gridless row"
        );
        assert_eq!(row.grid, Some((9, 9)));
    }

    #[test]
    fn interiors_and_other_worldspaces_are_refused() {
        let directory = database();
        let connection = connection(&directory);
        let interior = find_cell_by_editor_id(&connection, "whiterunbanneredmare", 60)
            .unwrap()
            .unwrap();
        let error = exterior_target(&interior, 60, "WhiterunBanneredMare").unwrap_err();
        assert!(
            error.contains("interior cells are not supported yet"),
            "{error}"
        );
        let no_grid = find_cell_by_editor_id(&connection, "NoGrid", 60)
            .unwrap()
            .unwrap();
        let error = exterior_target(&no_grid, 60, "NoGrid").unwrap_err();
        assert!(error.contains("no grid position"), "{error}");
        let other = find_cell_by_editor_id(&connection, "OtherWorldCell", 60)
            .unwrap()
            .unwrap();
        let error = exterior_target(&other, 60, "OtherWorldCell").unwrap_err();
        assert!(error.contains("worldspace"), "{error}");
        let own = find_cell_by_editor_id(&connection, "Riverwood", 60)
            .unwrap()
            .unwrap();
        assert_eq!(exterior_target(&own, 60, "Riverwood"), Ok((4, -12)));
    }

    /// Expected values are written out from the engine's convention: the origin cell's centre is
    /// (2048, ground, -2048) (`setup_world`'s start position) and +grid_y runs towards -z.
    #[test]
    fn cell_centre_is_half_a_cell_in_from_the_corner_relative_to_the_origin() {
        for (grid, origin, ground, expected) in [
            (
                (0, 0),
                IVec2::new(0, 0),
                7.0,
                Vec3::new(2048.0, 7.0, -2048.0),
            ),
            (
                (0, 1),
                IVec2::new(0, 0),
                0.0,
                Vec3::new(2048.0, 0.0, -6144.0),
            ),
            (
                (1, 0),
                IVec2::new(0, 0),
                0.0,
                Vec3::new(6144.0, 0.0, -2048.0),
            ),
            (
                (-1, -1),
                IVec2::new(0, 0),
                0.0,
                Vec3::new(-2048.0, 0.0, 2048.0),
            ),
            (
                (4, -12),
                IVec2::new(4, -12),
                123.0,
                Vec3::new(2048.0, 123.0, -2048.0),
            ),
            (
                (6, -10),
                IVec2::new(4, -12),
                123.0,
                Vec3::new(10240.0, 123.0, -10240.0),
            ),
            (
                (2, -13),
                IVec2::new(4, -12),
                -5.5,
                Vec3::new(-6144.0, -5.5, 2048.0),
            ),
        ] {
            assert_eq!(
                cell_centre_feet(grid, origin, ground),
                expected,
                "grid {grid:?} origin {origin:?}"
            );
        }
    }

    /// A cell cache with flat terrain for cells 1 (height 0) and 2 (height 100); cell 5 has none.
    fn cell_cache(directory: &tempfile::TempDir) -> CellCache {
        let path = directory.path().join("cell_cache.rkyv");
        let land = |cell_id, height| shared::CachedLand {
            cell_id,
            width: 3,
            height: 3,
            heights: vec![height; 9],
            normals: vec![0; 27],
            vertex_colors: vec![255; 27],
            layers: vec![],
            water_height: None,
            water_type_form_id: None,
        };
        let source = shared::CellCache {
            version: shared::CELL_CACHE_VERSION,
            cells: vec![land(0, 200.0), land(1, 0.0), land(2, 100.0), land(9, 300.0)],
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&source).unwrap();
        std::fs::write(&path, bytes).unwrap();
        CellCache::open(&path).unwrap()
    }

    fn command_app(directory: &tempfile::TempDir) -> App {
        let assets_dir = directory.path().to_owned();
        let cache = cell_cache(directory);
        let mut app = headless::fixture_app_with(|app| {
            app.insert_resource(cache)
                .insert_resource(EngineConfig {
                    assets_dir,
                    worldspace_id: 60,
                    ..default()
                })
                .insert_resource(RenderOrigin(IVec2::new(4, -12)))
                .init_resource::<ActiveSpace>()
                .init_resource::<CameraSpace>()
                .init_resource::<StreamingWorld>()
                .init_resource::<StreamingMetrics>()
                .init_resource::<TerrainContinuity>()
                .init_resource::<ProfilingState>()
                .init_resource::<ConsoleState>();
            register_cell_commands(app);
        });
        app.update();
        app
    }

    fn scrollback(app: &App) -> String {
        app.world().resource::<ConsoleState>().scrollback.join("\n")
    }

    fn camera_position(app: &mut App) -> Vec3 {
        let mut query = app
            .world_mut()
            .query_filtered::<&Transform, With<StreamingCamera>>();
        query.single(app.world()).unwrap().translation
    }

    #[test]
    fn help_lists_both_commands_with_their_short_forms() {
        let directory = database();
        let app = command_app(&directory);
        let text = app
            .world()
            .resource::<crate::console::ConsoleRegistry>()
            .help_text(None);
        assert!(text.contains("CenterOnExterior <x> <y> [coe]"), "{text}");
        assert!(
            text.contains("CenterOnCell <cell editor id> [coc]"),
            "{text}"
        );
    }

    #[test]
    fn coe_moves_the_camera_to_the_cell_centre_and_bad_input_moves_nothing() {
        let directory = database();
        let mut app = command_app(&directory);
        let before = camera_position(&mut app);
        for line in [
            "coe",
            "coe 1",
            "coe a b",
            "coe 99 99",
            "coc",
            "coc ",
            "coc Nowhere",
            "coc WhiterunBanneredMare",
            "coc OtherWorldCell",
            "coc NoGrid",
            "coe 6 -12",
            "coc NoLand",
        ] {
            execute_line(app.world_mut(), line);
        }
        app.update();
        assert_eq!(camera_position(&mut app), before);
        let text = scrollback(&app);
        assert_eq!(text.matches("error:").count(), 12, "{text}");
        assert!(
            text.contains("interior cells are not supported yet"),
            "{text}"
        );
        assert_eq!(
            text.matches("cell 6 -12 has no terrain height").count(),
            2,
            "{text}"
        );

        execute_line(app.world_mut(), "COE 5 -12");
        app.update();
        // Feet at (6144, 100 + 16, -2048) in the render space around origin (4, -12): the
        // terrain sample plus the landing clearance. The camera sits 50.4 (capsule centre) +
        // 89.6 (eye height) above them.
        let at = camera_position(&mut app);
        let camera = Vec3::new(6144.0, 256.0, -2048.0);
        assert!((at - camera).length() < 1e-2, "{at:?} vs {camera:?}");

        execute_line(app.world_mut(), "coc riverwood");
        app.update();
        let at = camera_position(&mut app);
        let camera = Vec3::new(2048.0, 156.0, -2048.0);
        assert!((at - camera).length() < 1e-2, "{at:?} vs {camera:?}");
    }

    #[test]
    fn console_teleports_leave_an_interior_before_the_planner_reads_the_camera() {
        for command in ["coe 4 -12", "coc riverwood"] {
            let directory = database();
            let mut app = command_app(&directory);
            let root = app.world_mut().spawn_empty().id();
            app.world_mut()
                .resource_mut::<StreamingWorld>()
                .set_resident_for_test(CellKey::Interior(3), root);
            app.insert_resource(ActiveSpace {
                worldspace_id: Some(60),
                interior: Some(3),
            })
            .insert_resource(CameraSpace::Interior);

            execute_line(app.world_mut(), command);

            assert!(app.world().get_entity(root).is_err());
            assert_eq!(
                *app.world().resource::<ActiveSpace>(),
                ActiveSpace {
                    worldspace_id: Some(60),
                    interior: None,
                }
            );
            assert_eq!(
                *app.world().resource::<CameraSpace>(),
                CameraSpace::Exterior
            );
            let expected = Vec3::new(2048.0, 156.0, -2048.0);
            assert!((camera_position(&mut app) - expected).length() < 1e-2);
            app.update();
            assert!((camera_position(&mut app) - expected).length() < 1e-2);
        }
    }

    #[test]
    fn cell_commands_follow_a_door_worldspace_change_even_at_the_same_grid() {
        for (command, expected) in [
            ("coe 4 -12", Vec3::new(2048.0, 456.0, -2048.0)),
            ("coc riverwood", Vec3::new(14336.0, 356.0, -79872.0)),
        ] {
            let directory = database();
            let mut app = command_app(&directory);
            app.insert_resource(ActiveSpace {
                worldspace_id: Some(61),
                interior: None,
            });
            execute_line(app.world_mut(), command);
            app.update();
            assert!((camera_position(&mut app) - expected).length() < 1e-2);
            assert_eq!(
                app.world().resource::<ActiveSpace>().worldspace_id,
                Some(61)
            );
            assert!(!scrollback(&app).contains("error:"));
        }
    }

    #[test]
    fn a_door_crossing_and_invalid_landings_leave_the_active_space_untouched() {
        let directory = database();
        let mut app = command_app(&directory);
        let root = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .set_resident_for_test(CellKey::Interior(3), root);
        let inside = ActiveSpace {
            worldspace_id: Some(60),
            interior: Some(3),
        };
        app.insert_resource(inside)
            .insert_resource(CameraSpace::Interior)
            .insert_resource(DoorCrossing::holding_for_test());
        let before = camera_position(&mut app);
        for command in ["coe 4 -12", "coc riverwood"] {
            execute_line(app.world_mut(), command);
        }
        assert_eq!(
            scrollback(&app).matches("door crossing to finish").count(),
            2
        );
        assert_eq!(*app.world().resource::<ActiveSpace>(), inside);
        assert!(app.world().get_entity(root).is_ok());
        assert_eq!(camera_position(&mut app), before);

        app.insert_resource(DoorCrossing::default());
        execute_line(app.world_mut(), "coe 6 -12");
        assert_eq!(*app.world().resource::<ActiveSpace>(), inside);
        assert_eq!(
            *app.world().resource::<CameraSpace>(),
            CameraSpace::Interior
        );
        assert!(app.world().get_entity(root).is_ok());
        assert_eq!(camera_position(&mut app), before);
    }
}
