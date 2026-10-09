//! Localized plugins' NPC names resolve through their string tables (#195). Synthetic plugins
//! and tables; no game data.
use converter::esm::{EsmParser, strings::StringsSource};
use rusqlite::Connection;
use std::{
    fs,
    path::{Path, PathBuf},
};

const LOCALIZED: u32 = 0x80;

fn sub(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    [tag.as_slice(), &(bytes.len() as u16).to_le_bytes(), bytes].concat()
}
fn record(tag: &[u8; 4], id: u32, payload: Vec<u8>) -> Vec<u8> {
    [
        tag.as_slice(),
        &(payload.len() as u32).to_le_bytes(),
        &0u32.to_le_bytes(),
        &id.to_le_bytes(),
        &[0; 8],
        &payload,
    ]
    .concat()
}
fn plugin(root: &Path, name: &str, masters: &[&str], flags: u32, records: Vec<u8>) -> PathBuf {
    let mut header = sub(b"HEDR", &[0; 12]);
    for master in masters {
        header.extend(sub(b"MAST", format!("{master}\0").as_bytes()));
        header.extend(sub(b"DATA", &[0; 8]));
    }
    let path = root.join(name);
    let tes4 = [
        b"TES4".as_slice(),
        &(header.len() as u32).to_le_bytes(),
        &flags.to_le_bytes(),
        &[0; 12],
        &header,
    ]
    .concat();
    fs::write(&path, [tes4, records].concat()).unwrap();
    path
}
/// An NPC whose FULL is either a string ID (localized) or text.
fn npc(id: u32, editor_id: &str, full: Option<&[u8]>) -> Vec<u8> {
    let mut payload = sub(b"EDID", format!("{editor_id}\0").as_bytes());
    if let Some(full) = full {
        payload.extend(sub(b"FULL", full));
    }
    record(b"NPC_", id, payload)
}
/// A `.STRINGS` table in the on-disk layout.
fn strings_table(path: &Path, entries: &[(u32, &str)]) {
    let mut directory = Vec::new();
    let mut data = Vec::new();
    for (id, text) in entries {
        directory.extend(id.to_le_bytes());
        directory.extend((data.len() as u32).to_le_bytes());
        data.extend(text.as_bytes());
        data.push(0);
    }
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        [
            (entries.len() as u32).to_le_bytes().as_slice(),
            &(data.len() as u32).to_le_bytes(),
            &directory,
            &data,
        ]
        .concat(),
    )
    .unwrap();
}
fn names(db: &Path) -> Vec<(u32, Option<String>)> {
    let conn = Connection::open(db).unwrap();
    let mut statement = conn
        .prepare("SELECT id, full_name FROM npcs ORDER BY id")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn npc_names_resolve_through_the_winning_plugins_string_table() {
    let dir = tempfile::tempdir().unwrap();
    let (data, vfs) = (dir.path().join("Data"), dir.path().join("vfs"));
    fs::create_dir_all(&data).unwrap();
    let base = plugin(
        &data,
        "Base.esm",
        &[],
        0x1 | LOCALIZED,
        [
            npc(0x800, "Housecarl", Some(&1u32.to_le_bytes())),
            npc(0x801, "Hunter", Some(&2u32.to_le_bytes())),
            npc(0x802, "Nameless", Some(&0u32.to_le_bytes())),
            npc(0x803, "Unlisted", Some(&99u32.to_le_bytes())),
            npc(0x804, "NoFull", None),
            npc(0x805, "Malformed", Some(b"Ab")),
        ]
        .concat(),
    );
    // A plain plugin stores text, even when it overrides a localized master's record.
    let patch = plugin(
        &data,
        "Patch.esp",
        &["Base.esm"],
        0,
        npc(0x800, "Housecarl", Some(b"Lydia the Housecarl\0")),
    );
    // A localized override's ID belongs to its own table, not its master's.
    let translation = plugin(
        &data,
        "Translation.esp",
        &["Base.esm"],
        LOCALIZED,
        npc(0x801, "Hunter", Some(&1u32.to_le_bytes())),
    );
    // Archives store lowercase paths; loose files keep their shipped case.
    strings_table(
        &vfs.join("strings/base_english.strings"),
        &[(1, "Lydia"), (2, "Faendal")],
    );
    strings_table(
        &vfs.join("strings/base_french.strings"),
        &[(1, "Lydia (fr)")],
    );
    strings_table(
        &data.join("Strings/Translation_English.STRINGS"),
        &[(1, "Faendal the Hunter")],
    );
    let plugins = [base, patch, translation];
    let source = |language: &str| StringsSource {
        roots: vec![data.clone(), vfs.clone()],
        language: language.to_owned(),
    };

    let db = dir.path().join("english.db");
    EsmParser::convert_plugins_with_strings(&plugins, &db, &source("english")).unwrap();
    assert_eq!(
        names(&db),
        [
            (0x800, Some("Lydia the Housecarl".into())),
            (0x801, Some("Faendal the Hunter".into())),
            (0x802, None),
            (0x803, None),
            (0x804, None),
            (0x805, None),
        ]
    );

    // The language picks the table.
    let db = dir.path().join("french.db");
    EsmParser::convert_plugins_with_strings(&plugins[..1], &db, &source("french")).unwrap();
    assert_eq!(names(&db)[0], (0x800, Some("Lydia (fr)".into())));

    // Without any string tables a localized name is NULL, never its ID bytes read as text.
    let db = dir.path().join("no-tables.db");
    EsmParser::convert_plugins(&plugins[..1], &db).unwrap();
    assert!(names(&db).iter().all(|(_, name)| name.is_none()));
}
