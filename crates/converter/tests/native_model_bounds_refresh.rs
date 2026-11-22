use converter::esm::exporter::create_tables;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

fn write_glb(path: &Path, x: f32, sphere: Option<Value>) {
    write_glb_with(path, x, sphere, |_| {});
}

fn write_glb_with(path: &Path, x: f32, sphere: Option<Value>, configure: impl FnOnce(&mut Value)) {
    let mut document = json!({
        "asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0]}],
        "nodes":[{"mesh":0}],"meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
        "accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3",
                      "min":[x,0,0],"max":[x+2.0,2,0]}],
        "bufferViews":[{"buffer":0,"byteLength":36}],"buffers":[{"byteLength":36}]
    });
    if let Some(sphere) = sphere {
        document["extras"]["mudcrabRoomSphere"] = sphere;
    }
    configure(&mut document);
    let mut document = serde_json::to_vec(&document).unwrap();
    document.resize(document.len().next_multiple_of(4), b' ');
    let mut glb = b"glTF".to_vec();
    glb.extend(2u32.to_le_bytes());
    glb.extend((20 + document.len() as u32 + 8 + 36).to_le_bytes());
    glb.extend((document.len() as u32).to_le_bytes());
    glb.extend(b"JSON");
    glb.extend(document);
    glb.extend(36u32.to_le_bytes());
    glb.extend(b"BIN\0");
    for value in [x, 0.0, 0.0, x + 2.0, 0.0, 0.0, x, 2.0, 0.0] {
        glb.extend(value.to_le_bytes());
    }
    fs::write(path, glb).unwrap();
}

fn run_refresh(root: &Path, paths: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_room-model-bounds-refresh"))
        .arg(root)
        .args(paths)
        .output()
        .unwrap()
}

fn refresh(root: &Path, paths: &[&str]) {
    let result = run_refresh(root, paths);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn installed_native_winners_refresh_all_matching_bases_and_room_ownership_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("pack");
    fs::create_dir_all(root.join("meshes")).unwrap();
    let db = root.join("skyrim_world.db");
    let connection = Connection::open(&db).unwrap();
    create_tables(&connection).unwrap();
    connection.execute_batch(
        r#"INSERT INTO statics(id,model_path,flags) VALUES
              (10,'50%25fern.nif',0),(11,'Meshes\50%25FERN.GLB',0),(12,'unchanged.glb',0);
           INSERT INTO room_bounds(ref_id,cell_id,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,half_x,half_y,half_z) VALUES
              (100,7,0,0,0,0,0,0,4,4,4),(101,7,20,0,0,0,0,0,4,4,4),
              (102,8,0,0,0,0,0,0,4,4,4);
           INSERT INTO "references"(id,cell_id,base_form_id,is_exterior,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z) VALUES
              (200,7,10,0,10,0,0,0,0,0),(201,7,11,0,10,0,0,0,0,0),
              (202,8,12,0,10,0,0,0,0,0),(203,7,10,0,0,0,0,0,0,0);
           INSERT INTO reference_rooms VALUES(203,100,1);
           INSERT INTO room_graph_cells VALUES(7,1),(8,0);
           INSERT INTO room_graph_stats VALUES('assignment_version',1);
           INSERT INTO room_reference_rules VALUES
              (200,7,'auto',NULL),(201,7,'auto',NULL),(202,8,'auto',NULL),(203,7,'target',100);"#,
    ).unwrap();
    let assert_winner = |x: Option<f64>, sphere: Option<f64>, overlap: Option<u32>| {
        for reference in [200u32, 201] {
            // The same rule/placement/base join the engine uses for automatic ownership.
            let projection = connection
                .query_row(
                    r#"SELECT CASE WHEN s.bounds_valid=1 THEN s.bounds_min_x END, b.center_x
                   FROM room_reference_rules p JOIN "references" r ON r.id=p.reference_id
                   JOIN statics s ON s.id=r.base_form_id
                   LEFT JOIN room_model_bounds b ON b.base_form_id=r.base_form_id
                   WHERE p.reference_id=?1 AND p.assignment='auto'"#,
                    [reference],
                    |row| Ok((row.get::<_, Option<f64>>(0)?, row.get::<_, Option<f64>>(1)?)),
                )
                .unwrap();
            assert_eq!(projection, (x, sphere));
            let overlaps = connection
                .prepare_cached("SELECT room_ref FROM reference_rooms WHERE reference_id=?1 AND contained=0 ORDER BY room_ref")
                .unwrap()
                .query_map([reference], |row| row.get::<_, u32>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(overlaps, overlap.as_slice());
        }
    };
    let sphere = |x| json!({"version":1,"center":[x,1.0,0.0],"radius":2.0});
    let path = root.join("meshes/50%fern.glb");
    write_glb(&path, -10.0, Some(sphere(-9.0)));
    write_glb(
        &root.join("meshes/unchanged.glb"),
        -10.0,
        Some(sphere(-9.0)),
    );
    refresh(&root, &["meshes/50%fern.glb", "meshes/unchanged.glb"]);
    assert_winner(Some(-10.0), Some(-9.0), Some(100));

    write_glb(&path, 10.0, Some(sphere(11.0)));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = fs::metadata(&path).unwrap().permissions();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        // A privileged test runner can still read a mode-000 file.
        if fs::File::open(&path).is_err() {
            let result = run_refresh(&root, &["meshes/50%fern.glb"]);
            assert!(!result.status.success());
            assert!(String::from_utf8_lossy(&result.stderr).contains("failed to open"));
            assert_winner(Some(-10.0), Some(-9.0), Some(100));
        }
        fs::set_permissions(&path, permissions).unwrap();
    }
    connection
        .execute_batch(
            "CREATE TRIGGER fail_overlap BEFORE INSERT ON reference_rooms WHEN NEW.room_ref=101
             BEGIN SELECT RAISE(ABORT, 'overlap update failure'); END;",
        )
        .unwrap();
    let result = run_refresh(&root, &["meshes/50%fern.glb"]);
    assert!(!result.status.success());
    let error = String::from_utf8_lossy(&result.stderr);
    assert!(error.contains("overlap update failure"), "{error}");
    assert!(error.contains("Keep the pack offline"), "{error}");
    assert_winner(Some(-10.0), Some(-9.0), Some(100));
    connection
        .execute_batch("DROP TRIGGER fail_overlap")
        .unwrap();

    refresh(&root, &["meshes/50%fern.glb"]);
    assert_winner(Some(10.0), Some(11.0), Some(101));

    write_glb(&path, 10.0, Some(sphere(-9.0)));
    refresh(&root, &["meshes/50%fern.glb"]);
    assert_winner(Some(10.0), None, Some(101));

    write_glb(&path, 10.0, None);
    refresh(&root, &["meshes/50%fern.glb"]);
    assert_winner(Some(10.0), None, Some(101));

    write_glb(&path, 10.0, Some(sphere(11.0)));
    refresh(&root, &["meshes/50%fern.glb"]);
    assert_winner(Some(10.0), Some(11.0), Some(101));

    write_glb(
        &path,
        10.0,
        Some(json!({"version":1,"center":[11,1,0],"radius":0})),
    );
    refresh(&root, &["meshes/50%fern.glb"]);
    assert_winner(Some(10.0), None, Some(101));

    write_glb(&path, -10.0, Some(sphere(-9.0)));
    refresh(&root, &["meshes/50%fern.glb"]);
    assert_winner(Some(-10.0), Some(-9.0), Some(100));

    fs::remove_file(&path).unwrap();
    refresh(&root, &["meshes/50%fern.glb"]);
    assert_winner(None, None, None);

    write_glb(&path, 10.0, Some(sphere(11.0)));
    refresh(&root, &["meshes/50%fern.glb"]);
    assert_winner(Some(10.0), Some(11.0), Some(101));
    fs::write(&path, b"invalid GLB").unwrap();
    let result = run_refresh(&root, &["meshes/50%fern.glb"]);
    assert!(result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("invalid GLB container"));
    assert_winner(None, None, None);

    let retained = connection.query_row(
        "SELECT bounds_valid,center_x,room_ref FROM statics JOIN room_model_bounds ON base_form_id=id
         JOIN reference_rooms ON reference_id=202 WHERE id=12",
        [], |row| Ok((row.get::<_, bool>(0)?, row.get::<_, f64>(1)?, row.get::<_, u32>(2)?)),
    ).unwrap();
    assert_eq!(retained, (true, -9.0, 102));
    let certification = connection
        .query_row(
            "SELECT (SELECT complete FROM room_graph_cells WHERE cell_id=7),
                (SELECT complete FROM room_graph_cells WHERE cell_id=8),
                (SELECT target_ref FROM room_reference_rules WHERE reference_id=203),
                (SELECT contained FROM reference_rooms WHERE reference_id=203 AND room_ref=100)",
            [],
            |row| {
                Ok((
                    row.get::<_, bool>(0)?,
                    row.get::<_, bool>(1)?,
                    row.get::<_, u32>(2)?,
                    row.get::<_, bool>(3)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(certification, (true, false, 100, true));
}

#[test]
fn native_spheres_accept_shading_extensions_and_still_require_enclosure() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir(root.join("meshes")).unwrap();
    let connection = Connection::open(root.join("skyrim_world.db")).unwrap();
    create_tables(&connection).unwrap();
    connection
        .execute(
            "INSERT INTO statics(id,model_path,flags) VALUES (10,'wall.nif',0)",
            [],
        )
        .unwrap();
    let key = "meshes/wall.glb";
    for (additional, required, radius, expected) in [
        (None, false, 2.0, true),
        (None, true, 2.0, true),
        (Some("KHR_texture_transform"), false, 2.0, true),
        (None, false, 0.5, false),
        (Some("KHR_mesh_quantization"), false, 2.0, false),
        (Some("KHR_materials_unknown"), false, 2.0, false),
    ] {
        let sphere = json!({"version":1,"center":[1,1,0],"radius":radius});
        write_glb_with(&root.join(key), 0.0, Some(sphere), |doc| {
            let specular = "KHR_materials_specular";
            doc["materials"] = json!([{"extensions":{(specular):{"specularFactor":0.5}}}]);
            doc["meshes"][0]["primitives"][0]["material"] = json!(0);
            doc["extensionsUsed"] = json!(
                std::iter::once(specular)
                    .chain(additional)
                    .collect::<Vec<_>>()
            );
            if required {
                doc["extensionsRequired"] = json!([specular]);
            }
        });
        refresh(root, &[key]);
        let retained: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM room_model_bounds WHERE base_form_id=10)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            retained, expected,
            "{additional:?}, required={required}, radius={radius}"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn refresh_resolves_case_aliases_and_rejects_colliding_winners() {
    const KEY: &str = "meshes/architecture/wall.glb";
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let path = root.join("Meshes/Architecture/Wall.GLB");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let connection = Connection::open(root.join("skyrim_world.db")).unwrap();
    create_tables(&connection).unwrap();
    connection
        .execute(
            "INSERT INTO statics(id,model_path,flags) VALUES (10,'architecture/wall.nif',0)",
            [],
        )
        .unwrap();
    let projection = || {
        connection
            .query_row(
                "SELECT CASE WHEN bounds_valid=1 THEN bounds_min_x END FROM statics WHERE id=10",
                [],
                |row| row.get::<_, Option<f64>>(0),
            )
            .unwrap()
    };
    write_glb(&path, 10.0, None);
    refresh(root, &[KEY]);
    assert_eq!(projection(), Some(10.0));

    write_glb_with(&path, 10.0, None, |document| {
        document["scene"] = json!(1);
        document["scenes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"nodes":[1]}));
        document["nodes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"mesh":0,"translation":[100,0,0]}));
    });
    refresh(root, &[KEY]);
    assert_eq!(projection(), Some(10.0), "streaming loads scene zero");
    let collision = root.join(KEY);
    fs::create_dir_all(collision.parent().unwrap()).unwrap();
    match fs::File::create_new(&collision) {
        Ok(_) => {
            write_glb(&collision, 20.0, None);
            let result = run_refresh(root, &[KEY]);
            assert!(!result.status.success());
            assert!(String::from_utf8_lossy(&result.stderr).contains("multiple installed GLBs"));
            assert_eq!(projection(), Some(10.0));
            fs::remove_file(collision).unwrap();
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => panic!("cannot create collision fixture: {error}"),
    }

    fs::remove_file(path).unwrap();
    refresh(root, &[KEY]);
    assert_eq!(projection(), None);
}

#[cfg(unix)]
#[test]
fn refresh_rejects_an_escaping_mixed_case_glb_path() {
    const KEY: &str = "meshes/architecture/wall.glb";
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("pack");
    fs::create_dir_all(root.join("Meshes")).unwrap();
    let outside = directory.path().join("outside");
    fs::create_dir(&outside).unwrap();
    write_glb(&outside.join("Wall.GLB"), 20.0, None);
    let connection = Connection::open(root.join("skyrim_world.db")).unwrap();
    create_tables(&connection).unwrap();
    connection
        .execute("INSERT INTO statics(id,model_path,flags,bounds_valid,bounds_min_x) VALUES (10,'architecture/wall.nif',0,1,10)", [])
        .unwrap();
    let linked_dir = root.join("Meshes/Architecture");
    std::os::unix::fs::symlink(&outside, &linked_dir).unwrap();
    let result = run_refresh(&root, &[KEY]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("GLB escapes pack"));

    fs::remove_file(&linked_dir).unwrap();
    fs::create_dir(&linked_dir).unwrap();
    std::os::unix::fs::symlink(outside.join("Wall.GLB"), linked_dir.join("Wall.GLB")).unwrap();
    let result = run_refresh(&root, &[KEY]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("GLB escapes pack"));
    let projection: (bool, f64) = connection
        .query_row(
            "SELECT bounds_valid,bounds_min_x FROM statics WHERE id=10",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(projection, (true, 10.0));
}
