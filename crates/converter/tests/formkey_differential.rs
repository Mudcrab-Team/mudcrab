//! Independent permissive legacy packing versus the Mutagen-inspired FormKey contract.
//! Strict old record policies are tested with the retained real remapper in esm's unit tests.
//! Synthetic fixtures are generated in-process; the real-input audit is explicitly opt-in.
use converter::esm::binary::{parse_plugin_file, parse_plugin_metadata};
use sha2::{Digest, Sha256};
use shared::{
    form_key::FormKey,
    legacy_id_view::LegacyIdView,
    plugin_table::{PluginFacts, PluginFlags, PluginTable},
    source_form_id::{Resolution, SourceFormId, ZeroPolicy, resolve},
};
use std::collections::BTreeMap;

#[derive(Clone)]
struct Plugin {
    name: String,
    light: bool,
    masters: Vec<String>,
}

/// Allocate pack-local plugin IDs independently of the supplied legacy slot order.
fn table(plugins: &[Plugin]) -> PluginTable {
    PluginTable::fresh(
        plugins
            .iter()
            .map(|p| {
                (
                    p.name.clone(),
                    PluginFacts {
                        masters: p.masters.clone(),
                        flags: PluginFlags {
                            light: p.light,
                            ..Default::default()
                        },
                        header_version: Some(1.71),
                        ..Default::default()
                    },
                )
            })
            .collect(),
    )
    .unwrap()
}

/// Frozen old permissive header packer: source selectors address MAST, then self.
/// Scope: representable, valid-width identities only, not strict headers or link recovery.
/// This oracle never calls resolve, FormKey, LegacyIdView, or converter remapping.
fn old_pack(plugins: &[Plugin], supplier: usize, raw: u32) -> Option<u32> {
    if raw == 0 {
        return Some(0);
    }
    let p = &plugins[supplier];
    let name = p.masters.get((raw >> 24) as usize).unwrap_or(&p.name);
    let owner = plugins
        .iter()
        .position(|p| p.name.eq_ignore_ascii_case(name))?;
    let light = plugins[owner].light;
    let slot = plugins[..owner].iter().filter(|p| p.light == light).count();
    let local = raw & 0xffffff;
    if light {
        (slot < 4096 && local <= 0xfff).then_some(0xfe000000 | ((slot as u32) << 12) | local)
    } else {
        (slot < 254).then_some(((slot as u32) << 24) | local)
    }
}

/// Exercise the shared identity contract separately from the converter's resolver.
fn key(t: &PluginTable, p: &Plugin, raw: u32) -> Resolution {
    resolve(
        t,
        SourceFormId {
            raw,
            containing_plugin: t.id(&p.name).unwrap(),
        },
        ZeroPolicy::Null,
    )
}

/// Compare independent packed winner bytes against shared and production native identities.
fn compare(plugins: &[Plugin], records: &[(usize, u32, u32)]) -> Vec<u8> {
    let t = table(plugins);
    let converter_order = converter::esm::form_keys::FormKeyOrder::from_facts(
        plugins
            .iter()
            .map(|p| {
                (
                    p.name.clone(),
                    t.entry(t.id(&p.name).unwrap()).unwrap().facts.clone(),
                )
            })
            .collect(),
    )
    .unwrap();
    let order: Vec<_> = plugins.iter().map(|p| t.id(&p.name).unwrap()).collect();
    let view = LegacyIdView::from_load_order(&t, &order).unwrap();
    let mut old = BTreeMap::new();
    let mut new = BTreeMap::<FormKey, u32>::new();
    for &(supplier, raw, payload) in records {
        let expected = old_pack(plugins, supplier, raw).unwrap();
        let resolved = key(&t, &plugins[supplier], raw).key();
        let converter_key = converter_order
            .resolve(supplier, raw, ZeroPolicy::Null)
            .unwrap();
        assert_eq!(converter_key, resolved);
        let actual = converter_key.map_or(0, |k| {
            converter_order
                .legacy_view()
                .unwrap()
                .to_legacy_u32(k)
                .unwrap()
        });
        assert_eq!(actual, expected, "{}:{raw:08X}", plugins[supplier].name);
        assert_eq!(view.from_legacy_u32(actual).unwrap(), resolved);
        if expected != 0 {
            old.insert(expected, payload);
        }
        if let Some(k) = resolved {
            new.insert(k, payload);
        }
    }
    let new: BTreeMap<_, _> = new
        .into_iter()
        .map(|(k, v)| (view.to_legacy_u32(k).unwrap(), v))
        .collect();
    assert_eq!(
        new, old,
        "last supplier must win even when table-id order differs"
    );
    let encode = |rows: BTreeMap<u32, u32>| {
        rows.into_iter()
            .flat_map(|(id, v)| [id.to_le_bytes(), v.to_le_bytes()].concat())
            .collect::<Vec<_>>()
    };
    let bytes = encode(old);
    assert_eq!(encode(new), bytes);
    bytes
}

#[test]
/// Asymmetric master indices and interleaved owners retain overrides and null semantics.
fn synthetic_nonidentity_masters_lights_overrides_and_null() {
    let plugins = vec![
        Plugin {
            name: "Z.esm".into(),
            light: false,
            masters: vec![],
        },
        Plugin {
            name: "L.esp".into(),
            light: true,
            masters: vec!["Z.esm".into()],
        },
        Plugin {
            name: "A.esp".into(),
            light: false,
            masters: vec!["L.esp".into(), "Z.esm".into()],
        },
    ];
    let bytes = compare(
        &plugins,
        &[
            (0, 0x800, 11),
            (1, 0x01000fff, 12),
            (2, 0x01000800, 99),
            (2, 0x00000fff, 88),
            (2, 0x02000801, 77),
            (2, 0, 66),
            (0, 0x00ffffff, 55),
        ],
    );
    assert_eq!(&bytes[..8], &[0, 8, 0, 0, 99, 0, 0, 0]);
}

#[test]
/// Missing owners and wide light locals remain distinct from usable neighboring identities.
fn malformed_master_and_light_width_are_distinguished_from_valid_neighbors() {
    let plugins = vec![
        Plugin {
            name: "Base.esm".into(),
            light: false,
            masters: vec![],
        },
        Plugin {
            name: "Patch.esl".into(),
            light: true,
            masters: vec!["Absent.esm".into()],
        },
    ];
    let t = table(&plugins);
    assert!(matches!(
        key(&t, &plugins[1], 0x800),
        Resolution::UnknownOwner(_)
    ));
    assert!(matches!(
        key(&t, &plugins[1], 0x01001000),
        Resolution::InvalidWidth { .. }
    ));
    let native = converter::esm::form_keys::FormKeyOrder::from_facts(vec![
        ("Base.esm".into(), PluginFacts::default()),
        (
            "Patch.esl".into(),
            PluginFacts {
                masters: vec!["Base.esm".into()],
                ..Default::default()
            },
        ),
    ])
    .unwrap();
    assert!(
        native
            .resolve(1, 0x7f000801, ZeroPolicy::Null)
            .unwrap()
            .is_some()
    );
    // The converter and untouched step-1 contract both select self beyond MAST.
    assert!(matches!(
        key(&t, &plugins[1], 0x7f000801),
        Resolution::SelfProjected { .. }
    ));
    assert!(key(&t, &plugins[0], 0x801).key().is_some());
    assert!(key(&t, &plugins[1], 0x01000802).key().is_some());
}

#[test]
/// Every native owner remains addressable when only checked legacy packing exceeds capacity.
fn synthetic_exact_and_exceeded_full_and_light_limits() {
    for (full, light) in [(254, 4096), (255, 4097)] {
        let plugins: Vec<_> = (0..full + light)
            .map(|n| Plugin {
                name: format!("P{n:05}.{}", if n < full { "esm" } else { "esl" }),
                light: n >= full,
                masters: vec![],
            })
            .collect();
        let t = table(&plugins);
        let order: Vec<_> = plugins.iter().map(|p| t.id(&p.name).unwrap()).collect();
        let view = LegacyIdView::from_load_order(&t, &order).unwrap();
        let mut resolved = 0;
        let mut refused = 0;
        for (i, p) in plugins.iter().enumerate() {
            let k = key(&t, p, 0xabc).key().unwrap();
            resolved += 1;
            match old_pack(&plugins, i, 0xabc) {
                Some(expected) => {
                    assert_eq!(view.to_legacy_u32(k), Ok(expected));
                    assert_eq!(view.from_legacy_u32(expected), Ok(Some(k)));
                }
                None => {
                    assert!(view.to_legacy_u32(k).is_err());
                    refused += 1;
                }
            }
        }
        assert_eq!(resolved, full + light);
        assert_eq!(refused, usize::from(full > 254) + usize::from(light > 4096));
    }
}

/// Interleaved light owners and alphabetic table allocation must retain unsigned Skyrim order.
#[test]
fn explicit_owner_priority_order_matches_literal_legacy_ids() {
    use converter::esm::form_keys::FormKeyOrder;
    let order = FormKeyOrder::from_facts(
        ["Z.esm", "A.esl", "C.esp", "Y.esl", "B.esp"]
            .map(|name| (name.to_owned(), PluginFacts::default()))
            .to_vec(),
    )
    .unwrap();
    let mut keys = [4, 3, 2, 1, 0].map(|supplier| {
        order
            .resolve(supplier, 0x812, ZeroPolicy::Identity)
            .unwrap()
            .unwrap()
    });
    keys.sort_unstable_by_key(|key| order.identity_sort_tuple(*key).unwrap());
    assert_eq!(
        keys[0].plugin().get(),
        5,
        "alphabetical registry allocation is not owner priority"
    );
    let actual = keys.map(|key| order.legacy_view().unwrap().to_legacy_u32(key).unwrap());
    assert_eq!(
        actual,
        [0x812, 0x0100_0812, 0x0200_0812, 0xfe00_0812, 0xfe00_1812]
    );
}

/// Encode synthetic native bytes independently of converter identity resolution.
fn native_record(tag: &[u8; 4], id: u32, flags: u32, body: &[u8]) -> Vec<u8> {
    [
        tag.as_slice(),
        &(body.len() as u32).to_le_bytes(),
        &flags.to_le_bytes(),
        &id.to_le_bytes(),
        &[0; 8],
        body,
    ]
    .concat()
}

/// Frame an authored field; fixtures use asymmetric IDs and nonalphabetical priorities.
fn native_field(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    [tag.as_slice(), &(bytes.len() as u16).to_le_bytes(), bytes].concat()
}

/// Frame a contextual CELL child group without using converter remapping.
fn cell_group(label: u32, records: &[u8]) -> Vec<u8> {
    [
        b"GRUP".as_slice(),
        &((records.len() + 24) as u32).to_le_bytes(),
        &label.to_le_bytes(),
        &6i32.to_le_bytes(),
        &[0; 8],
        records,
    ]
    .concat()
}

/// One format-2 VMAD object with distinct alias and padding sentinels.
fn script_object(raw: u32) -> Vec<u8> {
    let mut bytes = [5i16.to_le_bytes(), 2i16.to_le_bytes(), 1u16.to_le_bytes()].concat();
    bytes.extend(11u16.to_le_bytes());
    bytes.extend(b"Independent");
    bytes.push(0);
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(6u16.to_le_bytes());
    bytes.extend(b"Object");
    bytes.extend([1, 1, 0xa1, 0xb2, 0x56, 0x34]);
    bytes.extend(raw.to_le_bytes());
    bytes
}

/// Read every SQLite schema object and every table row, including virtual shadow tables.
fn logical_dump(path: &std::path::Path) -> Vec<String> {
    use rusqlite::{Connection, types::Value};
    let conn = Connection::open(path).unwrap();
    let schema: Vec<(String, String, Option<String>)> = conn
        .prepare("SELECT type,name,sql FROM sqlite_master ORDER BY type,name,sql")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let mut dump = Vec::new();
    for (kind, name, sql) in schema {
        dump.push(format!("schema {kind} {name} {sql:?}"));
        if kind != "table" {
            continue;
        }
        let mut query = conn
            .prepare(&format!("SELECT * FROM \"{}\"", name.replace('"', "\"\"")))
            .unwrap();
        let count = query.column_count();
        let mut rows: Vec<String> = query
            .query_map([], |row| {
                Ok(format!(
                    "{:?}",
                    (0..count)
                        .map(|i| row.get::<_, Value>(i))
                        .collect::<rusqlite::Result<Vec<_>>>()?
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        rows.sort();
        dump.extend(rows.into_iter().map(|row| format!("{name} {row}")));
    }
    dump
}

/// Compare actual conversion against frozen old packing, all-table SQL content and cache bytes.
#[test]
fn synthetic_publication_matches_independent_old_packer_and_cell_cache() {
    use converter::esm::{EsmParser, exporter, load_order::LoadOrder};
    use rusqlite::{Connection, params};
    let root = tempfile::tempdir().unwrap();
    let plugins = [
        Plugin {
            name: "Z.esm".into(),
            light: false,
            masters: vec![],
        },
        Plugin {
            name: "L.esl".into(),
            light: true,
            masters: vec!["Z.esm".into()],
        },
        Plugin {
            name: "A.esp".into(),
            light: false,
            masters: vec!["L.esl".into(), "Z.esm".into()],
        },
    ];
    let bodies = [
        native_record(b"STAT", 0x811, 0, &native_field(b"EDID", b"earlier\0")),
        native_record(b"STAT", 0x0100_0812, 0, &native_field(b"EDID", b"light\0")),
        [
            native_record(
                b"STAT",
                0x0100_0811,
                0,
                &[
                    native_field(b"EDID", b"winner\0"),
                    native_field(b"VMAD", &script_object(0x0000_0812)),
                ]
                .concat(),
            ),
            native_record(
                b"CELL",
                0x0200_0913,
                0,
                &[
                    native_field(b"EDID", b"InteriorFixture\0"),
                    native_field(b"DATA", &[1, 0]),
                ]
                .concat(),
            ),
            cell_group(
                0x0200_0913,
                &native_record(
                    b"REFR",
                    0x0200_0914,
                    0,
                    &[
                        native_field(b"NAME", &0x0100_0811u32.to_le_bytes()),
                        native_field(
                            b"XESP",
                            &[0x0000_0812u32.to_le_bytes(), [1, 0xa7, 0xb8, 0xc9]].concat(),
                        ),
                        native_field(b"DATA", &[0; 24]),
                    ]
                    .concat(),
                ),
            ),
        ]
        .concat(),
    ];
    let mut paths = Vec::new();
    for (p, body) in plugins.iter().zip(&bodies) {
        let mut header = native_field(
            b"HEDR",
            &[1.71f32.to_le_bytes(), [0; 4], 0x800u32.to_le_bytes()].concat(),
        );
        for master in &p.masters {
            header.extend(native_field(b"MAST", format!("{master}\0").as_bytes()));
            header.extend(native_field(b"DATA", &[0; 8]));
        }
        let path = root.path().join(&p.name);
        std::fs::write(
            &path,
            [
                native_record(b"TES4", 0, if p.light { 0x200 } else { 0 }, &header),
                body.clone(),
            ]
            .concat(),
        )
        .unwrap();
        paths.push(path);
    }
    let mut expected = std::collections::HashMap::new();
    for (supplier, path) in paths.iter().enumerate() {
        for mut record in parse_plugin_file(path).unwrap() {
            record.form_id = old_pack(&plugins, supplier, record.form_id).unwrap();
            if record.cell_form_id.is_some() {
                assert_eq!(record.cell_form_id, Some(0x0200_0913));
                record.cell_form_id = Some(0x0100_0913);
            }
            for (tag, data) in &mut record.subrecords {
                match tag.as_slice() {
                    b"VMAD" => *data = script_object(0xfe00_0812),
                    b"NAME" => *data = 0x811u32.to_le_bytes().to_vec(),
                    b"XESP" => data[..4].copy_from_slice(&0xfe00_0812u32.to_le_bytes()),
                    _ => {}
                }
            }
            record.load_order = supplier as u32;
            expected.insert(record.form_id, record);
        }
    }
    assert_eq!(expected[&0x811].load_order, 2);
    assert!(expected.contains_key(&0xfe00_0812));
    let old_db = root.path().join("old.db");
    let conn = Connection::open(&old_db).unwrap();
    exporter::create_tables(&conn).unwrap();
    for (priority, path) in paths.iter().enumerate() {
        conn.execute(
            "INSERT INTO plugins(id,name,priority,checksum) VALUES(?1,?2,?1,?3)",
            params![
                priority as u32,
                path.file_name().unwrap().to_string_lossy(),
                Sha256::digest(std::fs::read(path).unwrap()).as_slice()
            ],
        )
        .unwrap();
    }
    exporter::export_to_db_with_load_order(&conn, &expected, &LoadOrder::read(&paths).unwrap())
        .unwrap();
    drop(conn);
    let new_db = root.path().join("new.db");
    EsmParser::convert_plugins(&paths, &new_db).unwrap();
    assert_eq!(logical_dump(&old_db), logical_dump(&new_db));
    let actual = EsmParser::merge_plugins(&paths).unwrap();
    let old_cache = root.path().join("old.rkyv");
    let new_cache = root.path().join("new.rkyv");
    converter::esm::cell_cache::write_cell_cache(&expected, &old_cache).unwrap();
    converter::esm::cell_cache::write_cell_cache(&actual, &new_cache).unwrap();
    assert_eq!(
        std::fs::read(old_cache).unwrap(),
        std::fs::read(new_cache).unwrap()
    );
}

#[test]
#[ignore = "read-only 80-plugin audit; set FORMKEY_REAL80_AUDIT=1 explicitly"]
/// Optionally compare an installed source identity stream with a separately pinned receipt.
fn real80_record_identity_baseline_audit() {
    assert_eq!(std::env::var("FORMKEY_REAL80_AUDIT").as_deref(), Ok("1"));
    let input = std::path::PathBuf::from(
        std::env::var_os("FORMKEY_REAL80_INPUT").expect("read-only Data input"),
    );
    let list =
        std::path::PathBuf::from(std::env::var_os("FORMKEY_REAL80_PLUGINS").expect("plugins.txt"));
    let output = std::path::PathBuf::from(
        std::env::var_os("FORMKEY_REAL80_OUTPUT").expect("retained audit output"),
    );
    let paths = converter::esm::read_plugins_txt_with_options(
        &list,
        &input,
        Some(&input.join("Skyrim.ccc")),
        None,
    )
    .unwrap();
    assert_eq!(paths.len(), 80, "audit requires the pinned 80-plugin input");
    let plugins: Vec<_> = paths
        .iter()
        .map(|path| {
            let m = parse_plugin_metadata(path).unwrap();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            Plugin {
                light: name.to_ascii_lowercase().ends_with(".esl") || m.flags & 0x200 != 0,
                name,
                masters: m.masters,
            }
        })
        .collect();
    let mut records = Vec::new();
    let mut source_hashes = String::new();
    for (supplier, path) in paths.iter().enumerate() {
        let bytes = std::fs::read(path).unwrap();
        source_hashes.push_str(&format!(
            "{} {:x}\n",
            plugins[supplier].name,
            Sha256::digest(&bytes)
        ));
        for record in parse_plugin_file(path).unwrap() {
            if (record.form_id >> 24) as usize > plugins[supplier].masters.len() {
                assert_eq!(
                    record.record_type, *b"GMST",
                    "unexpected shipped invalid header"
                );
            }
            records.push((supplier, record.form_id, supplier as u32));
        }
    }
    let bytes = compare(&plugins, &records);
    let receipt = format!(
        "identity-winner-stream-sha256 {:x}\nsource-records {}\n{}",
        Sha256::digest(&bytes),
        records.len(),
        source_hashes
    );
    std::fs::create_dir_all(&output).unwrap();
    // Baselines are never silently overwritten. To pin a baseline, independently
    // review the candidate and copy it to real80-identity-baseline.txt in this folder.
    let baseline = output.join("real80-identity-baseline.txt");
    std::fs::write(output.join("real80-identity-candidate.txt"), &receipt).unwrap();
    assert_eq!(
        std::fs::read_to_string(baseline).expect("review and pin baseline before acceptance"),
        receipt
    );
}
