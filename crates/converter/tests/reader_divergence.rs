//! Synthetic examples of observed legacy and in-house reader differences. `reader_diff` reports
//! these differences conservatively; this fixture is not a correctness oracle or completeness
//! gate. See `docs/research/record-reader-divergence.md`.

use converter::reader_diff::{Options, Report, compare};
use std::{fs, path::Path, process::Command};

const TEXT: u32 = 0x0800;
const OUT_OF_ORDER: u32 = 0x0801;
const NAN_LIGHT: u32 = 0x0802;
const CHEST: u32 = 0x0803;
const CELL: u32 = 0x0900;
const DANGLING: u32 = 0x0A00;
/// The mod's records, in load-order numbering (the mod is the third plugin; it authors them with
/// its master count, 1, as the file index).
const MOD_IMAGE_SPACE: u32 = 0x0100_0801;
const LOCALIZED: u32 = 0x80;

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

fn light_data() -> Vec<u8> {
    let mut data = vec![0u8; 48];
    data[4..8].copy_from_slice(&256u32.to_le_bytes());
    data
}

fn master_plugin() -> Vec<u8> {
    let cell = record(
        b"CELL",
        CELL,
        &[
            sub(b"EDID", b"Cell\0"),
            sub(b"DATA", &[1, 0]),
            // XCLW is an f32; three bytes is a malformed field.
            sub(b"XCLW", &[0, 0, 0]),
        ]
        .concat(),
    );
    let reference = record(
        b"REFR",
        DANGLING,
        &[
            sub(b"NAME", &TEXT.to_le_bytes()),
            // XEMI names a light or region that does not exist.
            sub(b"XEMI", &0xDEADu32.to_le_bytes()),
            sub(b"DATA", &[0; 24]),
        ]
        .concat(),
    );
    [
        header(1, &[]),
        top(
            b"STAT",
            &[
                // Windows-1252 "Café".
                record(
                    b"STAT",
                    TEXT,
                    &[sub(b"EDID", b"Caf\xe9\0"), sub(b"MODL", b"a.nif\0")].concat(),
                ),
                // MODL before EDID: out of the schema's order.
                record(
                    b"STAT",
                    OUT_OF_ORDER,
                    &[sub(b"MODL", b"b.nif\0"), sub(b"EDID", b"Late\0")].concat(),
                ),
            ]
            .concat(),
        ),
        top(
            b"LIGH",
            &record(
                b"LIGH",
                NAN_LIGHT,
                &[
                    sub(b"EDID", b"Light\0"),
                    sub(b"DATA", &light_data()),
                    sub(b"FNAM", &f32::NAN.to_le_bytes()),
                ]
                .concat(),
            ),
        ),
        top(
            b"CONT",
            &record(
                b"CONT",
                CHEST,
                &[
                    sub(b"EDID", b"Chest\0"),
                    sub(b"MODL", b"chest.nif\0"),
                    sub(b"DATA", &[0, 0, 0, 0, 0]),
                ]
                .concat(),
            ),
        ),
        top(
            b"CELL",
            &group(
                2,
                0,
                &group(
                    3,
                    0,
                    &[cell, group(6, CELL, &group(9, CELL, &reference))].concat(),
                ),
            ),
        ),
    ]
    .concat()
}

/// A localized mod after an unrelated master.
fn mod_plugin() -> Vec<u8> {
    let image_space = MOD_IMAGE_SPACE.to_le_bytes();
    [
        header(LOCALIZED, &["Skyrim.esm"]),
        // A CONT override whose mandatory DATA is four bytes instead of five.
        top(
            b"CONT",
            &record(
                b"CONT",
                CHEST,
                &[
                    sub(b"EDID", b"Chest\0"),
                    sub(b"MODL", b"mod_chest.nif\0"),
                    sub(b"DATA", &[0, 0, 0, 0]),
                ]
                .concat(),
            ),
        ),
        top(
            b"IMGS",
            &record(b"IMGS", MOD_IMAGE_SPACE, &sub(b"EDID", b"ModImageSpace\0")),
        ),
        // IMSP names the mod's image space; the legacy reader does not remap IMSP.
        top(
            b"WTHR",
            &record(
                b"WTHR",
                0x0100_0802,
                &[
                    sub(b"EDID", b"ModWeather\0"),
                    sub(
                        b"IMSP",
                        &[image_space, image_space, image_space, image_space].concat(),
                    ),
                ]
                .concat(),
            ),
        ),
        // FULL is string ID 6 in Mod_english.STRINGS.
        top(
            b"ACTI",
            &record(
                b"ACTI",
                0x0100_0803,
                &[
                    sub(b"EDID", b"Rock\0"),
                    sub(b"FULL", &6u32.to_le_bytes()),
                    sub(b"MODL", b"rock.nif\0"),
                ]
                .concat(),
            ),
        ),
    ]
    .concat()
}

fn strings_table(entries: &[(u32, &[u8])]) -> Vec<u8> {
    let mut directory = Vec::new();
    let mut data = Vec::new();
    for (id, text) in entries {
        directory.extend(id.to_le_bytes());
        directory.extend(u32::try_from(data.len()).unwrap().to_le_bytes());
        data.extend(*text);
        data.push(0);
    }
    let mut bytes = u32::try_from(entries.len()).unwrap().to_le_bytes().to_vec();
    bytes.extend(u32::try_from(data.len()).unwrap().to_le_bytes());
    bytes.extend(directory);
    bytes.extend(data);
    bytes
}

fn write_inputs(root: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let data = root.join("Data");
    fs::create_dir_all(data.join("Strings")).unwrap();
    fs::write(data.join("Skyrim.esm"), master_plugin()).unwrap();
    fs::write(data.join("Other.esm"), header(1, &[])).unwrap();
    fs::write(data.join("Mod.esp"), mod_plugin()).unwrap();
    fs::write(
        data.join("Strings").join("Mod_english.STRINGS"),
        strings_table(&[(6, b"Rock")]),
    )
    .unwrap();
    let plugins = root.join("plugins.txt");
    fs::write(&plugins, "*Other.esm\n*Mod.esp\n").unwrap();
    (data, plugins)
}

fn build(root: &Path) -> Report {
    let (data, plugins) = write_inputs(root);
    let paths = converter::esm::read_plugins_txt(&plugins, &data).unwrap();
    assert_eq!(
        paths
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        ["Skyrim.esm", "Other.esm", "Mod.esp"]
    );
    let legacy = root.join("legacy.db");
    converter::esm::EsmParser::convert_plugins(&paths, &legacy).unwrap();
    let inhouse = root.join("inhouse");
    converter::esm::inhouse::export_record_bundle(&data, &plugins, &inhouse).unwrap();
    compare(
        &legacy,
        &inhouse.join("skyrim_world.db"),
        &Options::default(),
    )
    .unwrap()
}

fn field_keys(report: &Report, class: &str, group: &str) -> Vec<String> {
    report
        .records
        .fields
        .get(class)
        .and_then(|groups| groups.get(group))
        .map(|bucket| {
            bucket
                .samples
                .iter()
                .map(|sample| sample.key.clone())
                .collect()
        })
        .unwrap_or_default()
}

fn any_field_keys(report: &Report, group: &str) -> Vec<String> {
    report
        .records
        .fields
        .values()
        .filter_map(|groups| groups.get(group))
        .flat_map(|bucket| bucket.samples.iter().map(|sample| sample.key.clone()))
        .collect()
}

fn id(value: u32) -> String {
    format!("{value:08X}")
}

#[test]
fn reader_differences_are_reported_without_untyped_causal_labels() {
    let root = tempfile::tempdir().unwrap();
    let report = build(root.path());
    eprintln!("{}", converter::reader_diff::summary(&report));

    // Text bytes differ, but the comparator has no field type provenance to assign a cause.
    assert_eq!(field_keys(&report, "other_length", "STAT EDID"), [id(TEXT)]);
    // A known subrecord out of the schema's order is dropped by the in-house decoder.
    assert_eq!(
        field_keys(&report, "omitted_by_inhouse", "STAT EDID"),
        [id(OUT_OF_ORDER)]
    );
    // A field of the wrong size, and a non-finite float, are dropped by the in-house decoder.
    assert_eq!(
        field_keys(&report, "omitted_by_inhouse", "CELL XCLW"),
        [id(CELL)]
    );
    assert_eq!(
        field_keys(&report, "omitted_by_inhouse", "LIGH FNAM"),
        [id(NAN_LIGHT)]
    );
    // A link-shaped value is reported as bytes without reliable schema context.
    assert_eq!(
        field_keys(&report, "other_bytes", "REFR XEMI"),
        [id(DANGLING)]
    );
    // Current main remaps this typed link in both readers, so it converges.
    assert!(any_field_keys(&report, "WTHR IMSP").is_empty());
    // Current main resolves localized text in both readers, so it converges.
    assert!(any_field_keys(&report, "ACTI FULL").is_empty());
    // A candidate whose mandatory field is malformed loses to the earlier winner in-house only.
    let winners = &report.records.records["different_winner"]["CONT"];
    assert_eq!(winners.samples[0].key, id(CHEST));
    assert_eq!(
        report.tables["statics"].columns["model_path"].samples[0].legacy,
        "\"mod_chest.nif\""
    );
}

#[test]
fn build_refuses_existing_output_root_before_either_writer_runs() {
    let root = tempfile::tempdir().unwrap();
    let (data, plugins) = write_inputs(root.path());
    let output = root.path().join("existing-output");
    let legacy = output.join("legacy/skyrim_world.db");
    let inhouse_marker = output.join("inhouse/keep.txt");
    fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    fs::create_dir_all(inhouse_marker.parent().unwrap()).unwrap();
    fs::write(&legacy, b"legacy database that must not change").unwrap();
    fs::write(&inhouse_marker, b"existing in-house output").unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_reader-diff"))
        .arg("build")
        .arg(&data)
        .arg(&plugins)
        .arg(&output)
        .output()
        .unwrap();

    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("the root must not already exist"));
    assert!(!stderr.contains("legacy reader:"));
    assert_eq!(
        fs::read(&legacy).unwrap(),
        b"legacy database that must not change"
    );
    assert_eq!(
        fs::read(&inhouse_marker).unwrap(),
        b"existing in-house output"
    );
}

#[test]
fn build_creates_fresh_output_root_and_writes_both_synthetic_databases() {
    let root = tempfile::tempdir().unwrap();
    let (data, plugins) = write_inputs(root.path());
    let output = root.path().join("new-output");
    let report = root.path().join("reader-diff-report.json");

    let result = Command::new(env!("CARGO_BIN_EXE_reader-diff"))
        .arg("build")
        .arg(&data)
        .arg(&plugins)
        .arg(&output)
        .arg("--json")
        .arg(&report)
        .output()
        .unwrap();

    assert!(
        result.status.success(),
        "reader-diff build failed:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(output.join("legacy/skyrim_world.db").is_file());
    assert!(output.join("inhouse/skyrim_world.db").is_file());
    let report: serde_json::Value = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
    assert!(report.get("records").is_some());
}
