//! Local Mutagen comparison. Oracle JSONL and game files stay outside the repository.
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure},
};
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

#[derive(Debug, Deserialize)]
struct Oracle {
    form: String,
    #[serde(rename = "type")]
    kind: String,
    winner: String,
    #[serde(rename = "base")]
    base: String,
    pos: Option<[f64; 3]>,
    rot: Option<[f64; 3]>,
    scale: Option<f64>,
    #[serde(deserialize_with = "deserialize_record_flags")]
    flags: u32,
}

fn deserialize_record_flags<'de, D>(deserializer: D) -> std::result::Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = i64::deserialize(deserializer)?;
    u32::try_from(value)
        .or_else(|_| i32::try_from(value).map(|signed| signed as u32))
        .map_err(serde::de::Error::custom)
}

#[derive(Debug)]
struct Placed {
    kind: String,
    winner: String,
    base: u32,
    pos: [f64; 3],
    rot: [f64; 3],
    scale: f64,
}

fn form_id(key: &str, slots: &HashMap<String, u32>) -> Result<u32> {
    let (local, plugin) = key
        .split_once(':')
        .ok_or_else(|| color_eyre::eyre::eyre!("invalid FormKey {key}"))?;
    let local = u32::from_str_radix(local, 16)?;
    let prefix = *slots
        .get(&plugin.to_ascii_lowercase())
        .ok_or_else(|| color_eyre::eyre::eyre!("oracle names unloaded plugin {plugin}"))?;
    ensure!(
        local
            <= if prefix >> 24 == 0xFE {
                0xFFF
            } else {
                0xFF_FFFF
            },
        "FormKey local ID exceeds plugin width: {key}"
    );
    Ok(prefix | local)
}

fn close(actual: f64, expected: f64, tolerance: f64) -> bool {
    actual.is_finite() && expected.is_finite() && (actual - expected).abs() <= tolerance
}

fn compare(o: &Oracle, actual: Option<&Placed>, slots: &HashMap<String, u32>) -> Result<bool> {
    ensure!(
        slots.contains_key(&o.winner.to_ascii_lowercase()),
        "oracle winner is unloaded: {}",
        o.winner
    );
    if o.flags & 0x20 != 0 {
        ensure!(actual.is_none(), "deleted override was resurrected");
        return Ok(false);
    }
    let a = actual.ok_or_else(|| color_eyre::eyre::eyre!("non-deleted reference is missing"))?;
    ensure!(a.kind == o.kind, "record type mismatch");
    ensure!(
        a.winner.eq_ignore_ascii_case(&o.winner),
        "winning plugin mismatch"
    );
    ensure!(a.base == form_id(&o.base, slots)?, "base FormID mismatch");
    let pos = o
        .pos
        .ok_or_else(|| color_eyre::eyre::eyre!("live oracle row lacks position"))?;
    let rot = o
        .rot
        .ok_or_else(|| color_eyre::eyre::eyre!("live oracle row lacks rotation"))?;
    for axis in 0..3 {
        ensure!(
            close(a.pos[axis], pos[axis], 0.01),
            "position axis {axis} mismatch"
        );
        let expected = rot[axis];
        let delta = (a.rot[axis] - expected + std::f64::consts::PI)
            .rem_euclid(std::f64::consts::TAU)
            - std::f64::consts::PI;
        ensure!(
            a.rot[axis].is_finite() && expected.is_finite() && delta.abs() <= 1e-4,
            "rotation axis {axis} mismatch"
        );
    }
    ensure!(
        close(a.scale, o.scale.unwrap_or(1.0), 1e-4),
        "scale mismatch"
    );
    Ok(true)
}

/// Independently assign slots from TES4 headers, and verify the DB belongs to these files.
fn plugin_slots(db: &Connection, data: &Path) -> Result<HashMap<String, u32>> {
    let mut statement = db.prepare("SELECT name, checksum FROM plugins ORDER BY priority")?;
    let plugins = statement.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
    })?;
    let mut full = 0u32;
    let mut light = 0u32;
    let mut slots = HashMap::new();
    for plugin in plugins {
        let (name, checksum) = plugin?;
        let bytes =
            std::fs::read(data.join(&name)).wrap_err_with(|| format!("read plugin {name}"))?;
        ensure!(
            Sha256::digest(&bytes).as_slice() == checksum,
            "database source checksum differs: {name}"
        );
        ensure!(
            bytes.len() >= 12 && &bytes[..4] == b"TES4",
            "invalid plugin header: {name}"
        );
        let flags = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let prefix = if name.to_ascii_lowercase().ends_with(".esl") || flags & 0x200 != 0 {
            ensure!(light < 4096, "too many light plugins");
            let prefix = 0xFE00_0000 | (light << 12);
            light += 1;
            prefix
        } else {
            ensure!(full < 254, "too many full plugins");
            let prefix = full << 24;
            full += 1;
            prefix
        };
        ensure!(
            slots.insert(name.to_ascii_lowercase(), prefix).is_none(),
            "duplicate plugin {name}"
        );
    }
    ensure!(!slots.is_empty(), "database contains no plugin provenance");
    Ok(slots)
}

#[test]
#[ignore = "requires external Mutagen JSONL and matching game plugins; see docs/testing-mutagen.md"]
fn v122_current_references_match_mutagen_winning_overrides() -> Result<()> {
    let env = |name| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .ok_or_else(|| color_eyre::eyre::eyre!("set {name} for the Mutagen comparison"))
    };
    let oracle = env("MUDCRAB_MUTAGEN_ORACLE")?;
    let data = env("MUDCRAB_MUTAGEN_DATA")?;
    let temp = tempfile::tempdir()?;
    let db_path = if let Some(path) = std::env::var_os("MUDCRAB_MUTAGEN_DATABASE") {
        PathBuf::from(path)
    } else {
        let plugins = converter::esm::read_plugins_txt(&env("MUDCRAB_MUTAGEN_PLUGINS")?, &data)?;
        let path = temp.path().join("skyrim_world.db");
        converter::esm::EsmParser::convert_plugins(&plugins, &path)?;
        path
    };
    let db = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let slots = plugin_slots(&db, &data)?;
    // The oracle covers REFR/ACHR. PGRE and other placed types have no oracle row here.
    let mut statement = db.prepare(
        "SELECT f.id, r.record_type, p.name, f.base_form_id,
        f.pos_x, f.pos_y, f.pos_z, f.rot_x, f.rot_y, f.rot_z, f.scale
        FROM \"references\" f JOIN records r ON r.form_id=f.id
        JOIN plugins p ON p.priority=r.load_order WHERE r.record_type IN ('REFR','ACHR')",
    )?;
    let mut actual: HashMap<u32, Placed> = statement
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                Placed {
                    kind: r.get(1)?,
                    winner: r.get(2)?,
                    base: r.get(3)?,
                    pos: [r.get(4)?, r.get(5)?, r.get(6)?],
                    rot: [r.get(7)?, r.get(8)?, r.get(9)?],
                    scale: r.get(10)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut seen = HashSet::new();
    let (mut matched, mut deleted, mut failures) = (0usize, 0usize, 0usize);
    let mut examples = Vec::new();
    for (line, row) in BufReader::new(File::open(oracle)?).lines().enumerate() {
        let o: Oracle =
            serde_json::from_str(&row?).wrap_err_with(|| format!("oracle line {}", line + 1))?;
        ensure!(
            matches!(o.kind.as_str(), "REFR" | "ACHR"),
            "unsupported oracle type {}",
            o.kind
        );
        let id = form_id(&o.form, &slots)?;
        ensure!(seen.insert(id), "duplicate oracle FormKey {}", o.form);
        let row = actual.remove(&id);
        match compare(&o, row.as_ref(), &slots) {
            Ok(true) => matched += 1,
            Ok(false) => deleted += 1,
            Err(error) => {
                failures += 1;
                if examples.len() < 10 {
                    examples.push(format!("{} ({}): {error:#}", o.form, o.winner));
                }
            }
        }
    }
    ensure!(!seen.is_empty(), "oracle is empty");
    eprintln!(
        "Mutagen: {} rows, {matched} matched, {deleted} deleted overrides omitted, {failures} mismatched, {} unexpected REFR/ACHR rows",
        seen.len(),
        actual.len()
    );
    ensure!(
        failures == 0 && actual.is_empty(),
        "Mutagen comparison failed: {failures} mismatches, {} unexpected rows; {examples:?}",
        actual.len()
    );
    Ok(())
}

#[test]
fn v122_oracle_comparison_rejects_resurrection_mismatches_and_nonfinite_values() {
    let slots = HashMap::from([("base.esm".into(), 0), ("patch.esl".into(), 0xFE00_2000)]);
    assert_eq!(form_id("000801:Patch.esl", &slots).unwrap(), 0xFE00_2801);
    assert!(form_id("001801:Patch.esl", &slots).is_err());
    assert!(form_id("000801:Missing.esm", &slots).is_err());
    let mut o: Oracle = serde_json::from_str(r#"{"form":"000800:Base.esm","type":"REFR","winner":"Base.esm","base":"000801:Base.esm","pos":[1,2,3],"rot":[0,0,0],"scale":null,"flags":0}"#).unwrap();
    let mut a = Placed {
        kind: "REFR".into(),
        winner: "Base.esm".into(),
        base: 0x801,
        pos: [1.0, 2.0, 3.0],
        rot: [0.0; 3],
        scale: 1.0,
    };
    assert!(compare(&o, Some(&a), &slots).unwrap());
    assert!(compare(&o, None, &slots).is_err());
    a.rot[2] = std::f64::consts::TAU;
    assert!(compare(&o, Some(&a), &slots).unwrap());
    for field in [
        "base", "position", "rotation", "scale", "winner", "type", "nan",
    ] {
        let mut bad = Placed {
            kind: a.kind.clone(),
            winner: a.winner.clone(),
            ..a
        };
        match field {
            "base" => bad.base += 1,
            "position" => bad.pos[0] += 1.0,
            "rotation" => bad.rot[0] += 0.1,
            "scale" => bad.scale += 0.1,
            "winner" => bad.winner = "Patch.esl".into(),
            "type" => bad.kind = "ACHR".into(),
            _ => bad.pos[1] = f64::NAN,
        }
        assert!(compare(&o, Some(&bad), &slots).is_err(), "{field}");
    }
    o.flags = 0x20;
    assert!(!compare(&o, None, &slots).unwrap());
    assert!(compare(&o, Some(&a), &slots).is_err());
}

#[test]
fn v122_rejects_missing_live_placement_and_unloaded_deleted_winner() {
    let slots = HashMap::from([("base.esm".into(), 0)]);
    let mut o: Oracle = serde_json::from_str(r#"{"form":"000800:Base.esm","type":"REFR","winner":"Base.esm","base":"000801:Base.esm","pos":[0,0,0],"rot":[0,0,0],"scale":null,"flags":0}"#).unwrap();
    let a = Placed {
        kind: "REFR".into(),
        winner: "Base.esm".into(),
        base: 0x801,
        pos: [0.0; 3],
        rot: [0.0; 3],
        scale: 1.0,
    };
    assert!(compare(&o, Some(&a), &slots).unwrap());
    o.pos = None;
    assert!(compare(&o, Some(&a), &slots).is_err());
    o.pos = Some([0.0; 3]);
    o.rot = None;
    assert!(compare(&o, Some(&a), &slots).is_err());
    o.flags = 0x20;
    assert!(!compare(&o, None, &slots).unwrap());
    o.winner = "Missing.esm".into();
    assert!(compare(&o, None, &slots).is_err());
}

#[test]
fn v123_oracle_flags_preserve_signed_and_unsigned_32_bit_masks() {
    let mut row = serde_json::json!({"form":"000800:Base.esm","type":"REFR","winner":"Base.esm","base":"000801:Base.esm","pos":[0,0,0],"rot":[0,0,0],"scale":null,"flags":0});
    for (json, expected) in [
        (-2147482624i64, 0x80000400u32),
        (-2147483616, 0x80000020),
        (-2147483648, 0x80000000),
        (4294967295, 0xFFFFFFFF),
        (2147483647, 0x7FFFFFFF),
    ] {
        row["flags"] = serde_json::json!(json);
        let parsed: Oracle = serde_json::from_value(row.clone()).unwrap();
        assert_eq!(parsed.flags, expected);
    }
    row["flags"] = serde_json::json!(-2147483616i64);
    let parsed: Oracle = serde_json::from_value(row.clone()).unwrap();
    assert!(!compare(&parsed, None, &HashMap::from([("base.esm".into(), 0)])).unwrap());
    for invalid in [
        serde_json::json!(-2147483649i64),
        serde_json::json!(4294967296i64),
        serde_json::json!(1.5),
        serde_json::json!("32"),
    ] {
        row["flags"] = invalid;
        assert!(serde_json::from_value::<Oracle>(row.clone()).is_err());
    }
}
