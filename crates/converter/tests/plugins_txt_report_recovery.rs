//! Recovery reports retain every excluded original filename and the published slot order.
use converter::{AssetPipeline, PipelineConfig, esm::exporter::validate_database};
use dummy_content::layout;
use std::{fs, io::Write, path::Path};

/// Select one real production entry point without sharing its ordering implementation.
#[derive(Clone, Copy, Debug)]
enum Mode {
    Player,
    Automatic,
    Mo2,
}

/// Encode a bounded synthetic subrecord independently of the parser.
fn subrecord(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    [tag.as_slice(), &(bytes.len() as u16).to_le_bytes(), bytes].concat()
}

/// Encode a complete native record with independently chosen identity and flags.
fn record(tag: &[u8; 4], flags: u32, id: u32, payload: Vec<u8>) -> Vec<u8> {
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

/// Encode distinguishable plugin content, dependency names and full/light classes.
fn plugin(flags: u32, masters: &[&str], editor: &str, value: f32) -> Vec<u8> {
    let mut header = Vec::new();
    for master in masters {
        header.extend(subrecord(b"MAST", format!("{master}\0").as_bytes()));
        header.extend(subrecord(b"DATA", &[0; 8]));
    }
    let mut bytes = record(b"TES4", flags, 0, header);
    bytes.extend(record(
        b"GMST",
        0,
        ((masters.len() as u32) << 24) | 0x800,
        [
            subrecord(b"EDID", format!("{editor}\0").as_bytes()),
            subrecord(b"DATA", &value.to_le_bytes()),
        ]
        .concat(),
    ));
    bytes
}

/// Write a distinct actual filename, refusing an unsupported filesystem explicitly.
fn write_distinct(root: &Path, name: &str, bytes: &[u8]) {
    let mut file = fs::File::options()
        .write(true)
        .create_new(true)
        .open(root.join(name))
        .unwrap_or_else(|error| panic!("distinct plugin filename {name:?} required: {error}"));
    file.write_all(bytes).unwrap();
}

/// Verify missing-master, unusable-header and ambiguous recovery in player selection.
#[tokio::test]
#[cfg_attr(
    target_os = "macos",
    ignore = "requires distinct K and Kelvin-sign filenames"
)]
async fn player_report_includes_all_recovery_skips_and_final_slots() {
    for reverse_creation in [false, true] {
        assert_recovery_report(Mode::Player, true, reverse_creation).await;
    }
}

/// Verify all recovery causes retain automatic candidate order across recovery stages.
#[tokio::test]
#[cfg_attr(
    target_os = "macos",
    ignore = "requires distinct K and Kelvin-sign filenames"
)]
async fn automatic_report_includes_all_recovery_skips_and_final_slots() {
    for reverse_creation in [false, true] {
        assert_recovery_report(Mode::Automatic, true, reverse_creation).await;
    }
}

/// Verify header/master and later cycle recovery interleave correctly in profile order.
#[tokio::test]
async fn mo2_report_includes_header_master_and_cycle_skips_and_final_slots() {
    assert_recovery_report(Mode::Mo2, false, false).await;
}

/// A same-layer ambiguous plugin identity must preserve usable MO2 neighbors and both names.
#[tokio::test]
#[cfg_attr(
    target_os = "macos",
    ignore = "requires distinct K and Kelvin-sign filenames"
)]
async fn mo2_ambiguous_plugins_preserve_neighbor_publication_and_report() {
    for reverse_creation in [false, true] {
        assert_recovery_report(Mode::Mo2, true, reverse_creation).await;
    }
}

/// Publish real synthetic assets and compare report fields with independent fixture expectations.
async fn assert_recovery_report(mode: Mode, ambiguous: bool, reverse_creation: bool) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let data = root.join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(&data, layout::DEFAULT_SEED, layout::Formats::all()).unwrap();
    // A readable empty CCC is authoritative, so fallback warnings cannot mask recovery behavior.
    fs::write(root.join("Skyrim.ccc"), "").unwrap();
    let instance = root.join("MO2");
    let plugins_root = match mode {
        Mode::Mo2 => {
            for folder in ["mods/Recovery", "profiles/Default", "overwrite"] {
                fs::create_dir_all(instance.join(folder)).unwrap();
            }
            fs::write(
                instance.join("ModOrganizer.ini"),
                "[General]\ngameName=Skyrim Special Edition\n",
            )
            .unwrap();
            fs::write(instance.join("profiles/Default/modlist.txt"), "+Recovery\n").unwrap();
            instance.join("mods/Recovery")
        }
        _ => data.clone(),
    };
    for (name, flags, masters, editor, value) in [
        ("CycleA.esp", 0, vec!["CycleB.esp"], "fRejectedCycleA", 11.0),
        (
            "Missing.esp",
            0,
            vec!["Absent.esm"],
            "fRejectedMissing",
            19.0,
        ),
        ("Good.esp", 0, vec!["Skyrim.esm"], "fJumpHeightMin", 67.0),
        ("Child.esp", 0, vec!["Corrupt.esp"], "fRejectedChild", 29.0),
        ("CycleB.esp", 0, vec!["CycleA.esp"], "fRejectedCycleB", 31.0),
        (
            "GoodLight.esp",
            0x200,
            vec!["Skyrim.esm"],
            "fRecoveryLight",
            83.0,
        ),
    ] {
        write_distinct(&plugins_root, name, &plugin(flags, &masters, editor, value));
    }
    write_distinct(&plugins_root, "Corrupt.esp", b"TES4 truncated");
    if !matches!(mode, Mode::Automatic) {
        write_distinct(
            &plugins_root,
            "Unchecked.esp",
            b"invalid but deliberately unchecked",
        );
    }
    if ambiguous {
        let first = plugin(0, &["Skyrim.esm"], "fRejectedCollisionFirst", 37.0);
        let second = plugin(0x200, &["Skyrim.esm"], "fRejectedCollisionSecond", 41.0);
        let pairs = if reverse_creation {
            [("\u{212A}.esp", &second), ("K.esp", &first)]
        } else {
            [("K.esp", &first), ("\u{212A}.esp", &second)]
        };
        for (name, bytes) in pairs {
            write_distinct(&plugins_root, name, bytes);
        }
        assert_ne!(first, second);
        assert_eq!(fs::read(plugins_root.join("K.esp")).unwrap(), first);
        assert_eq!(fs::read(plugins_root.join("\u{212A}.esp")).unwrap(), second);
        write_distinct(
            &plugins_root,
            "AmbigChild.esp",
            &plugin(0, &["K.esp"], "fRejectedAmbigChild", 43.0),
        );
    }
    let mut ordered = vec!["CycleA.esp", "Missing.esp"];
    if ambiguous {
        ordered.push("K.esp");
    }
    ordered.extend([
        "Good.esp",
        "Corrupt.esp",
        "Child.esp",
        "CycleB.esp",
        "GoodLight.esp",
    ]);
    if ambiguous {
        ordered.push("AmbigChild.esp");
    }
    let mut entries = ordered
        .iter()
        .map(|name| format!("*{name}\n"))
        .collect::<String>();
    entries.push_str("Unchecked.esp\n");
    let output = root.join("pack");
    let mut config = PipelineConfig::new(&data, &output);
    match mode {
        Mode::Player => {
            let list = root.join("plugins.txt");
            fs::write(&list, entries).unwrap();
            config.plugins_file = Some(list);
        }
        Mode::Mo2 => {
            fs::write(instance.join("profiles/Default/plugins.txt"), entries).unwrap();
            fs::write(
                instance.join("profiles/Default/loadorder.txt"),
                format!("Skyrim.esm\n{}\nUnchecked.esp\n", ordered.join("\n")),
            )
            .unwrap();
            config.mo2 = Some(mo2::Selection {
                instance_path: instance,
                profile: "Default".into(),
            });
        }
        Mode::Automatic => {}
    }
    config.no_lod = true;
    config.cpu_jobs = 2;
    config.io_jobs = 1;
    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let result = AssetPipeline::run_async(config, tx).await;
    drain.await.unwrap();
    assert!(
        result.is_ok(),
        "local plugin defects must preserve {mode:?} neighbor publication: {result:?}"
    );
    let report = result.unwrap();
    assert!(report.complete, "{report:?}");
    assert!(output.join("meshes/generated.glb").is_file());
    let connection = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
    validate_database(&connection).unwrap();
    let published: Vec<String> = connection
        .prepare("SELECT name FROM plugins ORDER BY priority")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(published, ["Skyrim.esm", "Good.esp", "GoodLight.esp"]);
    let good: f64 = connection
        .query_row(
            "SELECT value FROM movement_game_settings WHERE editor_id='fJumpHeightMin'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(good, 67.0);
    let payloads: Vec<Vec<u8>> = connection
        .prepare("SELECT data FROM records WHERE record_type='GMST'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let editors: Vec<String> = payloads
        .iter()
        .map(|payload| {
            let record = rkyv::from_bytes::<
                converter::esm::types::ArchivedRecordData,
                rkyv::rancor::Error,
            >(payload)
            .unwrap();
            let bytes = &record
                .subrecords
                .iter()
                .find(|subrecord| subrecord.tag == *b"EDID")
                .unwrap()
                .data;
            String::from_utf8(bytes.strip_suffix(&[0]).unwrap_or(bytes).to_vec()).unwrap()
        })
        .collect();
    assert!(editors.iter().any(|editor| editor == "fRecoveryLight"));
    assert!(
        !editors.iter().any(|editor| editor.starts_with("fRejected")),
        "{editors:?}"
    );
    let expected = if matches!(mode, Mode::Automatic) {
        vec![
            "AmbigChild.esp",
            "Child.esp",
            "Corrupt.esp",
            "CycleA.esp",
            "CycleB.esp",
            "K.esp",
            "\u{212A}.esp",
            "Missing.esp",
        ]
    } else {
        let mut names = vec!["CycleA.esp", "Missing.esp"];
        if ambiguous {
            names.extend(["K.esp", "\u{212A}.esp"]);
        }
        names.extend(["Corrupt.esp", "Child.esp", "CycleB.esp"]);
        if ambiguous {
            names.push("AmbigChild.esp");
        }
        names
    };
    assert_eq!(
        report.skipped_plugins, expected,
        "all present recovered inputs must be reported once in pre-recovery selection order ({mode:?})"
    );
    if ambiguous {
        assert_eq!(
            report
                .notices
                .iter()
                .filter(|notice| notice.contains("installed plugin name collision for k.esp;"))
                .count(),
            1,
            "{report:?}"
        );
    }
    // JSON access keeps this regression compilable before the new Rust report field exists.
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(
        json.get("load_order"),
        Some(&serde_json::json!(published)),
        "reported original filenames must match published slot order"
    );
    let mut old_json = json;
    old_json.as_object_mut().unwrap().remove("load_order");
    let old_report: converter::PipelineReport = serde_json::from_value(old_json).unwrap();
    assert!(
        serde_json::to_value(old_report)
            .unwrap()
            .get("load_order")
            .is_none(),
        "legacy reports must deserialize with an omitted empty load order"
    );
}
