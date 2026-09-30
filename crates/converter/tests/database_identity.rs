//! Synthetic ownership fixtures; no game data.
use converter::esm::{EsmParser, load_order::LoadOrder};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn sub(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    [tag.as_slice(), &(bytes.len() as u16).to_le_bytes(), bytes].concat()
}
fn record(tag: &[u8; 4], id: u32, flags: u32, payload: Vec<u8>) -> Vec<u8> {
    [
        tag.as_slice(),
        &(payload.len() as u32).to_le_bytes(),
        &flags.to_le_bytes(),
        &id.to_le_bytes(),
        &[0; 8],
        &payload,
    ]
    .concat()
}
fn plugin(root: &Path, name: &str, masters: &[&str], flags: u32, records: Vec<u8>) -> PathBuf {
    let mut header = Vec::new();
    for master in masters {
        header.extend(sub(b"MAST", format!("{master}\0").as_bytes()));
        header.extend(sub(b"DATA", &[0; 8]));
    }
    let path = root.join(name);
    fs::write(&path, [record(b"TES4", 0, flags, header), records].concat()).unwrap();
    path
}
fn grass(id: u32, density: u8) -> Vec<u8> {
    record(
        b"GRAS",
        id,
        0,
        sub(b"EDID", format!("Grass{density}\0").as_bytes()),
    )
}

#[test]
fn database_identities_survive_slot_changes_and_keep_override_provenance() {
    use converter::esm::exporter::{export_to_db, export_to_db_with_load_order};
    use rusqlite::Connection;
    let dir = tempfile::tempdir().unwrap();
    let base = plugin(dir.path(), "Base.esm", &[], 0, grass(0x800, 35));
    let filler = plugin(dir.path(), "Filler.esm", &[], 0, Vec::new());
    let light = plugin(
        dir.path(),
        "Light.esl",
        &["Base.esm"],
        0x200,
        grass(0x01000810, 50),
    );
    let light_filler = plugin(dir.path(), "Empty.esl", &[], 0x200, Vec::new());
    let patch = plugin(
        dir.path(),
        "Patch.esp",
        &["Base.esm", "Light.esl"],
        0,
        [grass(0x800, 70), grass(0x01000810, 80)].concat(),
    );
    for (index, paths, full_id, light_id) in [
        (
            0,
            vec![base.clone(), light.clone(), patch.clone()],
            0x800u32,
            0xfe000810u32,
        ),
        (
            1,
            vec![filler, base, light_filler, light, patch],
            0x01000800,
            0xfe001810,
        ),
    ] {
        let path = dir.path().join(format!("world{index}.db"));
        EsmParser::convert_plugins(&paths, &path).unwrap();
        let conn = Connection::open(path).unwrap();
        let read = |id: u32| {
            conn.query_row(
            "SELECT plugin_name, internal_id, load_order FROM formid_map JOIN records USING(form_id) WHERE form_id=?1",
            [id], |row| Ok((row.get::<_,String>(0)?, row.get::<_,u32>(1)?, row.get::<_,u32>(2)?))).unwrap()
        };
        let priority = paths.len() as u32 - 1;
        assert_eq!(read(full_id), ("base.esm".into(), 0x800, priority));
        assert_eq!(read(light_id), ("light.esl".into(), 0x810, priority));
        // An annotation-only export must preserve the established identity.
        let mut records = EsmParser::merge_plugins(&paths).unwrap();
        export_to_db(&conn, &records).unwrap();
        assert_eq!(read(full_id), ("base.esm".into(), 0x800, priority));
        // Unresolved ownership is an error, and rolls back the whole export.
        let mut invalid = records[&full_id].clone();
        invalid.form_id = 0xff000999;
        records.insert(invalid.form_id, invalid);
        assert!(
            export_to_db_with_load_order(&conn, &records, &LoadOrder::read(&paths).unwrap())
                .is_err()
        );
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM records WHERE form_id=?1",
                [0xff000999u32],
                |row| row.get::<_, u32>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(read(full_id), ("base.esm".into(), 0x800, priority));
    }
}

fn setting(id: u32, name: &str, value: f32, flags: u32) -> Vec<u8> {
    record(
        b"GMST",
        id,
        flags,
        [
            sub(b"EDID", format!("{name}\0").as_bytes()),
            sub(b"DATA", &value.to_le_bytes()),
        ]
        .concat(),
    )
}

#[test]
fn game_settings_override_by_editor_id_with_deletion_restoration_and_null_overrides() {
    let dir = tempfile::tempdir().unwrap();
    // The shipped GMST convention can use a high byte beyond the master table.
    let base = plugin(
        dir.path(),
        "Base.esm",
        &[],
        0,
        setting(0x0123c00e, "fJumpHeightMin", 76.0, 0),
    );
    let patch = plugin(
        dir.path(),
        "Patch.esp",
        &["Base.esm"],
        0,
        setting(0x01000810, "FJUMPHEIGHTMIN", 90.0, 0),
    );
    let delete = plugin(
        dir.path(),
        "Delete.esp",
        &["Base.esm"],
        0,
        setting(0x01000811, "fJumpHeightMin", 0.0, 0x20),
    );
    let restore = plugin(
        dir.path(),
        "Restore.esl",
        &["Base.esm"],
        0x200,
        setting(0, "fJumpHeightMin", 120.0, 0),
    );
    let mut paths = vec![base, patch];
    for (stage, expected) in [(0, Some(90.0)), (1, None), (2, Some(120.0))] {
        if stage == 1 {
            paths.push(delete.clone());
        }
        if stage == 2 {
            paths.push(restore.clone());
        }
        let merged = EsmParser::merge_plugins(&paths).unwrap();
        assert_eq!(merged.len(), usize::from(expected.is_some()));
        let db = dir.path().join(format!("gmst{stage}.db"));
        EsmParser::convert_plugins(&paths, &db).unwrap();
        let conn = rusqlite::Connection::open(db).unwrap();
        if let Some(expected) = expected {
            assert_eq!(merged[&0x23c00e].load_order, paths.len() as u32 - 1);
            let (id, value): (u32, f64) = conn
                .query_row(
                    "SELECT id,value FROM movement_game_settings WHERE editor_id='fJumpHeightMin'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(id, 0x23c00e);
            assert_eq!(value, expected);
            assert_eq!(
                conn.query_row(
                    "SELECT plugin_name FROM formid_map WHERE form_id=?1",
                    [id],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
                "base.esm"
            );
        } else {
            assert_eq!(
                conn.query_row("SELECT count(*) FROM movement_game_settings", [], |row| row
                    .get::<_, u32>(0))
                    .unwrap(),
                0
            );
        }
    }
}

#[test]
fn ambiguous_new_game_setting_ids_fail_instead_of_overwriting_another_record() {
    for bytes in [
        [
            setting(0x800, "fFirst", 1.0, 0),
            setting(0x800, "fSecond", 2.0, 0),
        ]
        .concat(),
        [grass(0x800, 1), setting(0x800, "fFirst", 1.0, 0)].concat(),
        [setting(0x800, "fFirst", 1.0, 0), grass(0x800, 1)].concat(),
        setting(0, "fUnknown", 1.0, 0),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let base = plugin(dir.path(), "Base.esm", &[], 0, bytes);
        assert!(EsmParser::merge_plugins(&[base]).is_err());
    }
}
