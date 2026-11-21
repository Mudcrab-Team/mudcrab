//! Both record readers publish the same reference links and details (#269 gap 3).

use super::EsmParser;
use crate::config::RecordReader;
use rusqlite::{Connection, types::Value};
use std::{collections::HashMap, fs, path::Path};

const KEYWORD: u32 = 0x0800;
const FACTION: u32 = 0x0801;
const KEY: u32 = 0x0802;
const LOCATION: u32 = 0x0803;
const ZONE: u32 = 0x0804;
const CRATE: u32 = 0x0805;
const DOOR_BASE: u32 = 0x0806;
const DESTINATION: u32 = 0x0A04;
const CELL: u32 = 0x0900;
const CHEST: u32 = 0x0A00;
const LEVER: u32 = 0x0A01;
const TRAP: u32 = 0x0A02;
const MARKER: u32 = 0x0A03;
const PLATE_ACRE: u32 = 0x0A05;
const PLATE_ACHR: u32 = 0x0A06;
const PLATE_VALID: u32 = 0x0A07;
const ACTOR: u32 = 0x0807;
const CREATURE: u32 = 0x0808;
const DELETED_REFERENCE: u32 = 0x0809;
const ACTOR_BASE: u32 = 0x080A;
const LOW_WRONG_TYPE: u32 = 0x15;
/// The mod's own key and door, authored with its master count (1) as the file index, and their
/// load-order numbers: the mod is the third plugin.
const MOD_KEY_AUTHORED: u32 = 0x0100_0800;
const MOD_KEY: u32 = 0x0200_0800;
const MOD_DOOR_AUTHORED: u32 = 0x0100_0801;
const MOD_DOOR: u32 = 0x0200_0801;
const MOD_PLATE_AUTHORED: u32 = 0x0100_0802;
const MOD_PLATE: u32 = 0x0200_0802;

fn sub(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    let mut framed = tag.to_vec();
    framed.extend(u16::try_from(bytes.len()).unwrap().to_le_bytes());
    framed.extend(bytes);
    framed
}

fn record_with_flags(tag: &[u8; 4], id: u32, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut framed = tag.to_vec();
    framed.extend(u32::try_from(payload.len()).unwrap().to_le_bytes());
    framed.extend(flags.to_le_bytes());
    framed.extend(id.to_le_bytes());
    framed.extend(0u32.to_le_bytes());
    framed.extend(44u16.to_le_bytes());
    framed.extend(0u16.to_le_bytes());
    framed.extend(payload);
    framed
}

fn record(tag: &[u8; 4], id: u32, payload: &[u8]) -> Vec<u8> {
    record_with_flags(tag, id, 0, payload)
}

fn group(kind: u32, label: u32, payload: &[u8]) -> Vec<u8> {
    let mut framed = b"GRUP".to_vec();
    framed.extend((24 + u32::try_from(payload.len()).unwrap()).to_le_bytes());
    framed.extend(label.to_le_bytes());
    framed.extend(kind.to_le_bytes());
    framed.extend([0; 8]);
    framed.extend(payload);
    framed
}

fn top(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    group(0, u32::from_le_bytes(*tag), payload)
}

fn header(flags: u32, masters: &[&str]) -> Vec<u8> {
    let hedr = [
        1.7f32.to_le_bytes(),
        0u32.to_le_bytes(),
        0x800u32.to_le_bytes(),
    ]
    .concat();
    let mut payload = sub(b"HEDR", &hedr);
    for master in masters {
        payload.extend(sub(b"MAST", &[master.as_bytes(), &[0]].concat()));
        payload.extend(sub(b"DATA", &[0; 8]));
    }
    record_with_flags(b"TES4", 0, flags, &payload)
}

fn named(tag: &[u8; 4], id: u32) -> Vec<u8> {
    let editor_id = format!("{}{id:X}\0", String::from_utf8_lossy(tag));
    top(tag, &record(tag, id, &sub(b"EDID", editor_id.as_bytes())))
}

fn le(value: u32) -> [u8; 4] {
    value.to_le_bytes()
}

/// A placed crate with `extra` subrecords between its base and its position.
fn reference(id: u32, extra: &[Vec<u8>]) -> Vec<u8> {
    record(
        b"REFR",
        id,
        &[
            sub(
                b"NAME",
                &le(if id == MOD_DOOR_AUTHORED || id == DESTINATION {
                    DOOR_BASE
                } else {
                    CRATE
                }),
            ),
            extra.concat(),
            sub(b"DATA", &[0; 24]),
        ]
        .concat(),
    )
}

/// A 20-byte `XLOC`: level, three unused bytes, key, flags, then padding.
fn lock(level: u8, key: u32, flags: u8) -> Vec<u8> {
    [
        vec![level, 0, 0, 0],
        le(key).to_vec(),
        vec![flags, 0, 0, 0],
        vec![0; 8],
    ]
    .concat()
}

fn interior(references: &[u8]) -> Vec<u8> {
    interior_with_id(CELL, references)
}

fn interior_with_id(cell_id: u32, references: &[u8]) -> Vec<u8> {
    let cell = record(
        b"CELL",
        cell_id,
        &[sub(b"EDID", b"Vault\0"), sub(b"DATA", &[1, 0])].concat(),
    );
    top(
        b"CELL",
        &group(
            2,
            0,
            &group(
                3,
                0,
                &[cell, group(6, cell_id, &group(9, cell_id, references))].concat(),
            ),
        ),
    )
}

#[test]
fn plugin_deletion_disables_hardcoded_owner_fallback_across_subset_exports() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("Skyrim.esm");
    let patch = dir.path().join("Delete.esp");
    let deleted = 0x600;
    let absent = 0x601;
    let owner = |id| [sub(b"XOWN", &le(id)), sub(b"XRNK", &le(7))];
    fs::write(
        &base,
        [
            header(1, &[]),
            named(b"STAT", CRATE),
            named(b"FACT", deleted),
            interior(
                &[
                    reference(CHEST, &owner(deleted)),
                    reference(LEVER, &owner(absent)),
                ]
                .concat(),
            ),
        ]
        .concat(),
    )
    .unwrap();
    fs::write(
        &patch,
        [
            header(0, &["Skyrim.esm"]),
            top(b"FACT", &record_with_flags(b"FACT", deleted, 0x20, &[])),
        ]
        .concat(),
    )
    .unwrap();
    let restoration = dir.path().join("Restore.esp");
    fs::write(
        &restoration,
        [header(0, &["Skyrim.esm"]), named(b"FACT", deleted)].concat(),
    )
    .unwrap();
    let paths = [base, patch];
    let subset: HashMap<_, _> = EsmParser::merge_plugins(&paths)
        .unwrap()
        .into_iter()
        .filter(|(id, _)| [CHEST, LEVER].contains(id))
        .collect();
    for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
        let db_path = dir.path().join(format!("{reader:?}.db"));
        EsmParser::convert_plugins_with_reader(&paths, &db_path, reader, dir.path()).unwrap();
        let db = Connection::open(db_path).unwrap();
        let projected_owner = |id| {
            db.query_row(
                "SELECT owner_id FROM reference_details WHERE reference_id=?1",
                [id],
                |row| row.get::<_, Option<u32>>(0),
            )
            .unwrap()
        };
        assert_eq!(projected_owner(CHEST), None);
        assert_eq!(projected_owner(LEVER), Some(absent));
        super::exporter::export_to_db(&db, &subset).unwrap();
        assert_eq!(projected_owner(CHEST), None);
        assert_eq!(projected_owner(LEVER), Some(absent));

        let restored_path = dir.path().join(format!("{reader:?}-restored.db"));
        let restored_plugins = [paths[0].clone(), paths[1].clone(), restoration.clone()];
        let mut winners = EsmParser::merge_plugins(&restored_plugins).unwrap();
        let faction = winners.remove(&deleted).unwrap();
        super::exporter::export_to_db(&db, &HashMap::from([(deleted, faction)])).unwrap();
        assert_eq!(projected_owner(CHEST), Some(deleted));
        assert_eq!(projected_owner(LEVER), Some(absent));

        EsmParser::convert_plugins_with_reader(
            &restored_plugins,
            &restored_path,
            reader,
            dir.path(),
        )
        .unwrap();
        let restored = Connection::open(restored_path).unwrap();
        let owner: u32 = restored
            .query_row(
                "SELECT d.owner_id FROM reference_details d
                 JOIN records r ON r.form_id=d.owner_id AND r.record_type='FACT'
                 WHERE d.reference_id=?1 AND NOT EXISTS (
                     SELECT 1 FROM subset_nonplacement_deletions WHERE form_id=r.form_id
                 )",
                [CHEST],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(owner, deleted);
    }
}

#[test]
fn typed_owner_restoration_respects_authored_zero_and_rank() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("Skyrim.esm");
    let owner = |id| [sub(b"XOWN", &le(id)), sub(b"XRNK", &le(7))];
    fs::write(
        &base,
        [
            header(1, &[]),
            named(b"STAT", CRATE),
            interior(
                &[
                    reference(CHEST, &owner(FACTION)),
                    reference(LEVER, &owner(0)),
                    reference(TRAP, &owner(FACTION)),
                ]
                .concat(),
            ),
        ]
        .concat(),
    )
    .unwrap();
    let paths = [base];
    let mut records = EsmParser::merge_plugins(&paths).unwrap();
    let mut trap = records.remove(&TRAP).unwrap();
    trap.subrecords
        .iter_mut()
        .find(|(tag, _)| tag == b"XOWN")
        .unwrap()
        .1
        .fill(0);
    let mut target = records.remove(&CRATE).unwrap();
    target.form_id = FACTION;
    target.record_type = *b"FACT";
    let subset = HashMap::from([(TRAP, trap)]);
    let addition = HashMap::from([(FACTION, target)]);
    for (name, legacy_schema) in [("current.db", false), ("form-id-only.db", true)] {
        let db_path = dir.path().join(name);
        let publish = || {
            EsmParser::convert_plugins_with_reader(
                &paths,
                &db_path,
                RecordReader::Inhouse,
                dir.path(),
            )
            .unwrap();
        };
        publish();
        let db = Connection::open(&db_path).unwrap();
        let columns = || {
            db.query_row(
                "SELECT count(*) FROM pragma_table_info('reference_link_subset_sources')",
                [],
                |row| row.get::<_, usize>(0),
            )
            .unwrap()
        };
        let owner_rank = |id| {
            db.query_row(
                "SELECT owner_id,owner_rank FROM reference_details WHERE reference_id=?1",
                [id],
                |r| Ok((r.get::<_, Option<u32>>(0)?, r.get::<_, i32>(1)?)),
            )
            .unwrap()
        };
        assert_eq!(columns(), 0);
        if legacy_schema {
            db.execute_batch(
                "CREATE TABLE reference_link_subset_sources(form_id INTEGER PRIMARY KEY)",
            )
            .unwrap();
            db.execute(
                "INSERT INTO reference_link_subset_sources VALUES (?1)",
                [LEVER],
            )
            .unwrap();
        }
        assert_eq!(owner_rank(CHEST), (None, 7));
        super::exporter::export_to_db(&db, &subset).unwrap();
        assert_eq!(columns(), 2);
        if legacy_schema {
            let retained: bool = db
                .query_row(
                    "SELECT data IS NULL FROM reference_link_subset_sources WHERE form_id=?1",
                    [LEVER],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(retained);
        }
        assert_eq!(
            db.query_row(
                "SELECT form_id FROM reference_link_subset_sources WHERE form_id=?1",
                [TRAP],
                |row| { row.get::<_, u32>(0) }
            )
            .unwrap(),
            TRAP
        );
        super::exporter::export_to_db(&db, &addition).unwrap();
        assert_eq!(owner_rank(CHEST), (Some(FACTION), 7));
        assert_eq!(owner_rank(LEVER), (None, 7));
        assert_eq!(owner_rank(TRAP), (None, 7));
        drop(db);
        publish();
        let db = Connection::open(&db_path).unwrap();
        assert!(
            !db.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='reference_link_subset_sources')",
                [],
                |row| row.get::<_, bool>(0)
            )
            .unwrap()
        );
    }
}

#[test]
fn malformed_placement_enable_parent_keeps_predecessor() {
    use super::placement_edits::{PlacementSource, apply};

    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("Skyrim.esm");
    let db_path = dir.path().join("skyrim_world.db");
    let xesp = |id, flags| sub(b"XESP", &[le(id), [flags, 0, 0, 0]].concat());
    fs::write(
        &base,
        [
            header(1, &[]),
            named(b"STAT", CRATE),
            interior(
                &[
                    reference(CHEST, &[xesp(LEVER, 1)]),
                    reference(LEVER, &[]),
                    reference(TRAP, &[]),
                ]
                .concat(),
            ),
        ]
        .concat(),
    )
    .unwrap();
    EsmParser::convert_plugins_with_reader(&[base], &db_path, RecordReader::Inhouse, dir.path())
        .unwrap();
    let db = Connection::open(db_path).unwrap();
    let parent = || {
        db.query_row(
            "SELECT enable_parent_id,enable_parent_flags FROM [references] WHERE id=?1",
            [CHEST],
            |row| Ok((row.get::<_, Option<u32>>(0)?, row.get::<_, Option<u32>>(1)?)),
        )
        .unwrap()
    };
    let apply_patch = |name: &str, fields: &[Vec<u8>]| {
        let path = dir.path().join(name);
        fs::write(
            &path,
            [
                header(0, &["Skyrim.esm"]),
                group(6, CELL, &group(9, CELL, &reference(CHEST, fields))),
            ]
            .concat(),
        )
        .unwrap();
        apply(dir.path(), PlacementSource::SkyrimPlugin(&path)).unwrap()
    };

    assert_eq!(parent(), (Some(LEVER), Some(1)));
    assert_eq!(
        apply_patch("Malformed.esp", &[sub(b"XESP", &le(TRAP))]).changed_references,
        0
    );
    assert_eq!(parent(), (Some(LEVER), Some(1)));
    assert_eq!(
        apply_patch("BadMaster.esp", &[xesp(0x9900_0802, 2)]).changed_references,
        0
    );
    assert_eq!(parent(), (Some(LEVER), Some(1)));
    assert_eq!(
        apply_patch("Repeated.esp", &[xesp(TRAP, 2), xesp(LEVER, 1)]).changed_references,
        0
    );
    assert_eq!(parent(), (Some(LEVER), Some(1)));
    assert_eq!(
        apply_patch("Changed.esp", &[xesp(TRAP, 2)]).changed_references,
        1
    );
    assert_eq!(parent(), (Some(TRAP), Some(2)));
    assert_eq!(apply_patch("Cleared.esp", &[]).changed_references, 1);
    assert_eq!(parent(), (None, None));
}

fn master_plugin() -> Vec<u8> {
    let chest = reference(
        CHEST,
        &[
            sub(b"XLKR", &[le(KEYWORD), le(LEVER)].concat()),
            sub(b"XLKR", &le(TRAP)),
            sub(b"XLKR", &[le(KEYWORD), le(0xDEAD)].concat()),
            sub(b"XLKR", &le(0x14)),
            sub(b"XLKR", &le(0)),
            sub(b"XLKR", &[le(ACTOR), le(LEVER)].concat()),
            sub(b"XLKR", &le(CREATURE)),
            sub(b"XLKR", &le(DELETED_REFERENCE)),
            sub(b"XLKR", &le(LOW_WRONG_TYPE)),
            sub(b"XLKR", &[le(0), le(TRAP)].concat()),
            sub(b"XAPD", &[1]),
            sub(
                b"XAPR",
                &[le(LEVER).to_vec(), 0.5f32.to_le_bytes().to_vec()].concat(),
            ),
            sub(
                b"XAPR",
                &[le(KEY).to_vec(), 1.0f32.to_le_bytes().to_vec()].concat(),
            ),
            sub(
                b"XAPR",
                &[le(0x14).to_vec(), 0.25f32.to_le_bytes().to_vec()].concat(),
            ),
            sub(
                b"XAPR",
                &[le(0).to_vec(), 0.75f32.to_le_bytes().to_vec()].concat(),
            ),
            sub(
                b"XAPR",
                &[le(CREATURE).to_vec(), 1.25f32.to_le_bytes().to_vec()].concat(),
            ),
            sub(
                b"XAPR",
                &[le(LOW_WRONG_TYPE).to_vec(), 1.5f32.to_le_bytes().to_vec()].concat(),
            ),
            sub(b"XLCN", &le(LOCATION)),
            sub(b"XEZN", &le(ZONE)),
            sub(b"XOWN", &le(FACTION)),
            sub(b"XRNK", &3i32.to_le_bytes()),
            sub(b"XLOC", &lock(50, KEY, 0x04)),
        ],
    );
    let marker = reference(
        MARKER,
        &[
            sub(b"XMRK", &[]),
            sub(b"FNAM", &[0x03]),
            sub(b"TNAM", &[5, 0]),
        ],
    );
    [
        header(1, &[]),
        named(b"KYWD", KEYWORD),
        named(b"KYWD", LOW_WRONG_TYPE),
        named(b"FACT", FACTION),
        named(b"KEYM", KEY),
        named(b"LCTN", LOCATION),
        named(b"ECZN", ZONE),
        top(
            b"STAT",
            &record(
                b"STAT",
                CRATE,
                &[sub(b"EDID", b"Crate\0"), sub(b"MODL", b"crate.nif\0")].concat(),
            ),
        ),
        named(b"DOOR", DOOR_BASE),
        named(b"NPC_", ACTOR_BASE),
        top(
            b"ACHR",
            &record(b"ACHR", ACTOR, &sub(b"NAME", &le(ACTOR_BASE))),
        ),
        top(
            b"ACRE",
            &record(b"ACRE", CREATURE, &sub(b"NAME", &le(ACTOR_BASE))),
        ),
        interior(
            &[
                chest,
                reference(LEVER, &[]),
                reference(TRAP, &[]),
                reference(DELETED_REFERENCE, &[]),
                marker,
                reference(DESTINATION, &[]),
                reference(PLATE_ACRE, &[sub(b"XATR", &le(CREATURE))]),
                reference(PLATE_ACHR, &[sub(b"XATR", &le(ACTOR))]),
                reference(PLATE_VALID, &[sub(b"XATR", &le(LEVER))]),
            ]
            .concat(),
        ),
    ]
    .concat()
}

/// Deletes a live reference from the earlier master plugin.
fn deletion_plugin() -> Vec<u8> {
    [
        header(0, &["Skyrim.esm"]),
        top(
            b"REFR",
            &record_with_flags(b"REFR", DELETED_REFERENCE, 0x20, &[]),
        ),
    ]
    .concat()
}

/// A mod after an unrelated master: its own key and a door into the master's cell, locked with it.
fn mod_plugin() -> Vec<u8> {
    let door = reference(
        MOD_DOOR_AUTHORED,
        &[
            sub(b"ONAM", &[]),
            sub(
                b"XTEL",
                &[
                    le(DESTINATION).to_vec(),
                    [1.0f32, 2.0, 3.0, 0.0, 0.0, 0.5]
                        .into_iter()
                        .flat_map(f32::to_le_bytes)
                        .collect(),
                    le(0).to_vec(),
                ]
                .concat(),
            ),
            sub(b"XLKR", &le(LEVER)),
            sub(
                b"XAPR",
                &[le(LEVER).to_vec(), 0.5f32.to_le_bytes().to_vec()].concat(),
            ),
            sub(b"XLOC", &lock(255, MOD_KEY_AUTHORED, 0)),
        ],
    );
    let plate = reference(MOD_PLATE_AUTHORED, &[sub(b"XATR", &le(MOD_DOOR_AUTHORED))]);
    [
        header(0, &["Skyrim.esm"]),
        named(b"KEYM", MOD_KEY_AUTHORED),
        interior(&[door, plate].concat()),
    ]
    .concat()
}

type Rows = Vec<(String, Vec<Value>)>;

fn rows(conn: &Connection) -> Rows {
    let mut rows = Rows::new();
    for (table, key) in [
        ("reference_linked_refs", "reference_id, position"),
        ("reference_activate_parents", "reference_id, position"),
        ("reference_details", "reference_id"),
        ("door_links", "ref_id"),
    ] {
        let mut statement = conn
            .prepare(&format!("SELECT * FROM {table} ORDER BY {key}"))
            .unwrap();
        let columns = statement.column_count();
        rows.extend(
            statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|index| row.get::<_, Value>(index))
                        .collect::<rusqlite::Result<Vec<_>>>()
                        .map(|values| (table.to_owned(), values))
                })
                .unwrap()
                .map(Result::unwrap),
        );
    }
    rows
}

fn publish(
    reader: RecordReader,
    root: &Path,
    plugins: &[&str],
) -> (Connection, HashMap<u32, super::records::RawRecord>) {
    let paths: Vec<_> = plugins.iter().map(|name| root.join(name)).collect();
    let database = root.join(format!("{reader:?}.db"));
    let records = EsmParser::convert_plugins_with_reader(&paths, &database, reader, root).unwrap();
    (Connection::open(database).unwrap(), records)
}

fn get<T: rusqlite::types::FromSql>(conn: &Connection, sql: &str) -> T {
    conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

#[test]
fn both_record_readers_publish_the_same_reference_links() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("Skyrim.esm"), master_plugin()).unwrap();
    fs::write(root.path().join("Other.esm"), deletion_plugin()).unwrap();
    fs::write(root.path().join("Mod.esp"), mod_plugin()).unwrap();

    for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
        let (before_deletion, records) = publish(reader, root.path(), &["Skyrim.esm"]);
        assert!(records.contains_key(&DELETED_REFERENCE));
        let live_link: Option<i64> = get(
            &before_deletion,
            &format!(
                "SELECT linked_ref_id FROM reference_linked_refs WHERE reference_id={CHEST} AND position=7"
            ),
        );
        assert_eq!(live_link, Some(DELETED_REFERENCE.into()));
    }

    let plugins = ["Skyrim.esm", "Other.esm", "Mod.esp"];
    let (legacy, legacy_records) = publish(RecordReader::Legacy, root.path(), &plugins);
    let (inhouse, inhouse_records) = publish(RecordReader::Inhouse, root.path(), &plugins);
    assert!(!legacy_records.contains_key(&DELETED_REFERENCE));
    assert!(!inhouse_records.contains_key(&DELETED_REFERENCE));
    let deleted_link = |conn: &Connection| -> Option<i64> {
        get(
            conn,
            &format!(
                "SELECT linked_ref_id FROM reference_linked_refs WHERE reference_id={CHEST} AND position=7"
            ),
        )
    };
    assert_eq!(deleted_link(&legacy), None);
    assert_eq!(deleted_link(&inhouse), None);
    assert_eq!(rows(&legacy), rows(&inhouse));

    let conn = inhouse;
    let destination: u32 = get(
        &conn,
        &format!("SELECT destination_ref_id FROM door_links WHERE ref_id={MOD_DOOR}"),
    );
    assert_eq!(destination, DESTINATION);
    let arrival: f64 = get(
        &conn,
        &format!("SELECT pos_x FROM door_links WHERE ref_id={MOD_DOOR}"),
    );
    assert_eq!(arrival, 1.0);
    let linked: u32 = get(
        &conn,
        &format!("SELECT linked_ref_id FROM reference_linked_refs WHERE reference_id={MOD_DOOR}"),
    );
    assert_eq!(linked, LEVER);
    let parent: u32 = get(
        &conn,
        &format!(
            "SELECT parent_ref_id FROM reference_activate_parents WHERE reference_id={MOD_DOOR}"
        ),
    );
    assert_eq!(parent, LEVER);
    let links: Vec<(i64, Option<i64>, Option<i64>)> = conn
        .prepare(&format!(
            "SELECT position, keyword_id, linked_ref_id FROM reference_linked_refs WHERE reference_id={CHEST} ORDER BY position"
        ))
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        links,
        vec![
            (0, Some(KEYWORD.into()), Some(LEVER.into())),
            (1, None, Some(TRAP.into())),
            (2, Some(KEYWORD.into()), None),
            (3, None, Some(0x14)),
            (4, None, None),
            (5, Some(ACTOR.into()), Some(LEVER.into())),
            (6, None, None),
            (7, None, None),
            (8, None, None),
            (9, None, Some(TRAP.into())),
        ],
        "positions remain stable for hardcoded, null, missing, deleted, and mistyped links"
    );
    let parents: Vec<(i64, Option<i64>, Option<f64>)> = conn
        .prepare(&format!(
            "SELECT position, parent_ref_id, delay FROM reference_activate_parents WHERE reference_id={CHEST} ORDER BY position"
        ))
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        parents,
        vec![
            (0, Some(LEVER.into()), Some(0.5)),
            (1, None, Some(1.0)),
            (2, Some(0x14), Some(0.25)),
            (3, None, Some(0.75)),
            (4, None, Some(1.25)),
            (5, None, Some(1.5)),
        ],
        "hardcoded player survives, while null, deleted, and mistyped targets stay null"
    );

    let detail = |id: u32, column: &str| -> Option<i64> {
        get(
            &conn,
            &format!("SELECT {column} FROM reference_details WHERE reference_id={id}"),
        )
    };
    assert_eq!(detail(CHEST, "owner_id"), Some(FACTION.into()));
    assert_eq!(detail(CHEST, "owner_rank"), Some(3));
    assert_eq!(detail(CHEST, "activate_parent_flags"), Some(1));
    assert_eq!(detail(CHEST, "lock_level"), Some(50));
    assert_eq!(detail(CHEST, "lock_key_id"), Some(KEY.into()));
    assert_eq!(detail(CHEST, "lock_flags"), Some(4));
    assert_eq!(detail(CHEST, "location_id"), Some(LOCATION.into()));
    assert_eq!(detail(CHEST, "encounter_zone_id"), Some(ZONE.into()));
    assert_eq!(detail(CHEST, "map_marker"), Some(0));
    assert_eq!(detail(MARKER, "map_marker"), Some(1));
    assert_eq!(detail(MARKER, "map_marker_flags"), Some(3));
    assert_eq!(detail(MARKER, "map_marker_type"), Some(5));
    assert_eq!(detail(MOD_DOOR, "open_by_default"), Some(1));
    assert_eq!(detail(MOD_DOOR, "lock_level"), Some(255));
    assert_eq!(
        detail(MOD_DOOR, "lock_key_id"),
        Some(MOD_KEY.into()),
        "the XLOC key is remapped through the mod's master list"
    );
    assert_eq!(
        detail(MOD_PLATE, "attached_ref_id"),
        Some(MOD_DOOR.into()),
        "XATR is remapped by both readers"
    );
    let forbidden_details: i64 = get(
        &conn,
        &format!(
            "SELECT count(*) FROM reference_details WHERE reference_id IN ({PLATE_ACRE}, {PLATE_ACHR})"
        ),
    );
    assert_eq!(forbidden_details, 0);
    assert_eq!(detail(PLATE_VALID, "attached_ref_id"), Some(LEVER.into()));

    let subset_ids = [CHEST, KEYWORD, FACTION, KEY, LOCATION, ZONE, LEVER, TRAP];
    let subset: HashMap<_, _> = legacy_records
        .into_iter()
        .filter(|(id, _)| subset_ids.contains(id))
        .collect();
    let subset_db = Connection::open_in_memory().unwrap();
    super::exporter::create_tables(&subset_db).unwrap();
    super::exporter::export_to_db(&subset_db, &subset).unwrap();
    let subset_lock: Option<i64> = get(
        &subset_db,
        &format!("SELECT lock_key_id FROM reference_details WHERE reference_id={CHEST}"),
    );
    assert_eq!(
        subset_lock, None,
        "subset exports have no load-order key remap"
    );
    let subset_link: Option<i64> = get(
        &subset_db,
        &format!(
            "SELECT linked_ref_id FROM reference_linked_refs WHERE reference_id={CHEST} AND position=0"
        ),
    );
    assert_eq!(subset_link, Some(LEVER.into()));
    let plain: i64 = get(
        &conn,
        &format!("SELECT count(*) FROM reference_details WHERE reference_id IN ({LEVER}, {TRAP})"),
    );
    assert_eq!(
        plain, 0,
        "references without any of these fields get no row"
    );
}

#[test]
fn typed_target_only_additions_create_sparse_link_details() {
    use super::exporter::{export_placed_subset_with_sources, export_to_db};
    use super::load_order::LoadOrder;

    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("Skyrim.esm");
    let patch = dir.path().join("Patch.esp");
    fs::write(
        &base,
        [
            header(1, &[]),
            named(b"STAT", CRATE),
            interior(
                &[
                    reference(CHEST, &[sub(b"XATR", &le(LEVER))]),
                    reference(TRAP, &[sub(b"XOWN", &le(FACTION))]),
                ]
                .concat(),
            ),
        ]
        .concat(),
    )
    .unwrap();
    let (db, _) = publish(RecordReader::Inhouse, dir.path(), &["Skyrim.esm"]);
    let details = || {
        db.prepare(
            "SELECT reference_id,attached_ref_id,owner_id FROM reference_details ORDER BY reference_id",
        ).unwrap().query_map([], |row| {
            Ok((row.get::<_, u32>(0)?, row.get::<_, Option<u32>>(1)?, row.get::<_, Option<u32>>(2)?))
        }).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap()
    };
    assert!(details().is_empty());

    fs::write(
        &patch,
        [
            header(0, &["Skyrim.esm"]),
            group(6, CELL, &group(9, CELL, &reference(LEVER, &[]))),
        ]
        .concat(),
    )
    .unwrap();
    let paths = [base.clone(), patch];
    let order = LoadOrder::read(&paths).unwrap();
    let mut decoded = crate::records::read_plugins(&paths, &order).unwrap();
    export_placed_subset_with_sources(
        &db,
        &[(LEVER, decoded.records.remove(&LEVER).unwrap())].into(),
        &[],
        &order,
    )
    .unwrap();
    assert_eq!(details(), [(CHEST, Some(LEVER), None)]);

    let owner = dir.path().join("Owner.esp");
    fs::write(
        &owner,
        [header(0, &["Skyrim.esm"]), named(b"FACT", FACTION)].concat(),
    )
    .unwrap();
    let mut merged = EsmParser::merge_plugins(&[base, owner]).unwrap();
    export_to_db(&db, &[(FACTION, merged.remove(&FACTION).unwrap())].into()).unwrap();
    assert_eq!(
        details(),
        [(CHEST, Some(LEVER), None), (TRAP, None, Some(FACTION))]
    );
}

fn plane_marker(id: u32, link: Option<u32>) -> Vec<u8> {
    let mut primitive = [0; 32];
    primitive[..4].copy_from_slice(&10.0f32.to_le_bytes());
    primitive[8..12].copy_from_slice(&10.0f32.to_le_bytes());
    primitive[28..].copy_from_slice(&le(3));
    let mut fields = [
        sub(b"NAME", &le(super::rooms::OCCLUSION_MARKER_BASE)),
        sub(b"XPRM", &primitive),
        sub(b"DATA", &[0; 24]),
    ]
    .concat();
    if let Some(link) = link {
        fields.extend(sub(b"XORD", &[0, 0, link, 0].map(le).concat()));
    }
    record(b"REFR", id, &fields)
}

#[test]
fn native_empty_occluder_slots_preserve_cell_completeness() {
    use super::placement_edits::{PlacementSource, apply};
    use super::rooms::OCCLUSION_MARKER_BASE;

    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("Skyrim.esm");
    let db_path = dir.path().join("skyrim_world.db");
    fs::write(
        &base,
        [
            header(1, &[]),
            named(b"STAT", OCCLUSION_MARKER_BASE),
            interior(&plane_marker(CHEST, None)),
        ]
        .concat(),
    )
    .unwrap();
    EsmParser::convert_plugins_with_reader(&[base], &db_path, RecordReader::Inhouse, dir.path())
        .unwrap();
    let db = Connection::open(db_path).unwrap();
    let complete = || {
        get::<bool>(
            &db,
            "SELECT complete FROM room_graph_cells WHERE cell_id=0x900",
        )
    };
    assert!(complete());

    let path = dir.path().join("occluder.json");
    let id = 0xff00_0800u32;
    let edit = |links| {
        serde_json::json!({
            "edits": [{
                "op": "put", "id": id, "cell": CELL, "base": OCCLUSION_MARKER_BASE,
                "position": [0, 0, 0],
                "shape": {"kind": "plane", "half_width": 8, "half_height": 8},
                "links": links
            }]
        })
        .to_string()
    };
    let source = edit([0; 4]);
    fs::write(&path, &source).unwrap();
    apply(dir.path(), PlacementSource::NativeJson(&path)).unwrap();
    assert!(complete());
    let has_links = || {
        get::<bool>(
            &db,
            "SELECT has_links FROM room_occluders WHERE ref_id=0xff000800",
        )
    };
    assert!(!has_links());
    assert_eq!(
        get::<Vec<u8>>(
            &db,
            "SELECT source FROM room_placement_layers ORDER BY id DESC LIMIT 1"
        ),
        source.as_bytes(),
    );

    fs::write(&path, edit([0, 0, CHEST, 0])).unwrap();
    apply(dir.path(), PlacementSource::NativeJson(&path)).unwrap();
    assert!(has_links());
    assert_eq!(
        db.query_row(
            "SELECT link_index,linked_ref FROM room_occluder_links WHERE ref_id=?1",
            [id],
            |row| Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?)),
        )
        .unwrap(),
        (2, CHEST),
    );
}

#[test]
fn typed_occluder_link_survives_target_only_addition_in_another_cell() {
    use super::exporter::export_placed_subset_with_sources;
    use super::load_order::LoadOrder;

    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("Skyrim.esm");
    let patch = dir.path().join("Patch.esp");
    let other_cell = CELL + 1;
    fs::write(
        &base,
        [
            header(1, &[]),
            named(b"STAT", super::rooms::OCCLUSION_MARKER_BASE),
            interior(&plane_marker(CHEST, Some(LEVER))),
            interior_with_id(other_cell, &plane_marker(TRAP, None)),
        ]
        .concat(),
    )
    .unwrap();
    let (db, _) = publish(RecordReader::Inhouse, dir.path(), &["Skyrim.esm"]);
    let complete = |cell| {
        db.query_row(
            "SELECT complete FROM room_graph_cells WHERE cell_id=?1",
            [cell],
            |row| row.get::<_, bool>(0),
        )
        .unwrap()
    };
    let verified = || {
        get::<bool>(
            &db,
            "SELECT value FROM room_graph_stats WHERE name='occlusion_links_verified'",
        )
    };
    assert!(!complete(CELL));
    assert!(complete(other_cell));
    assert!(verified());

    fs::write(
        &patch,
        [
            header(0, &["Skyrim.esm"]),
            group(
                6,
                other_cell,
                &group(9, other_cell, &plane_marker(LEVER, None)),
            ),
        ]
        .concat(),
    )
    .unwrap();
    let paths = [base, patch];
    let order = LoadOrder::read(&paths).unwrap();
    let mut decoded = crate::records::read_plugins(&paths, &order).unwrap();
    export_placed_subset_with_sources(
        &db,
        &[(LEVER, decoded.records.remove(&LEVER).unwrap())].into(),
        &[],
        &order,
    )
    .unwrap();
    assert!(complete(other_cell));
    assert!(verified());
    let incoming: (u32, u32, u32) = db
        .query_row(
            "SELECT link.ref_id,link.link_index,target.cell_id FROM room_occluder_links link
         JOIN room_occluders target ON target.ref_id=link.linked_ref WHERE link.linked_ref=?1",
            [LEVER],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(incoming, (CHEST, 2, other_cell));
}
