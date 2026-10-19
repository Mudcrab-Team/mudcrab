//! Plugin selection must exclude ambiguous files while preserving usable neighboring plugins.
use converter::esm::{EsmParser, binary::parse_plugin_metadata, read_plugins_txt_with_options};
use std::{fs, io::Write, path::Path};

/// Encode fixture subrecords independently of the selection implementation.
fn subrecord(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    [tag.as_slice(), &(bytes.len() as u16).to_le_bytes(), bytes].concat()
}

/// Encode a complete native header around a distinguishable fixture record.
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

/// Materialize a usable plugin with independent dependency, slot-class and content markers.
fn plugin_bytes(flags: u32, masters: &[&[u8]], editor: &str) -> Vec<u8> {
    let mut header = Vec::new();
    for &master in masters {
        header.extend(subrecord(b"MAST", &[master, &[0]].concat()));
        header.extend(subrecord(b"DATA", &[0; 8]));
    }
    let mut bytes = record(b"TES4", flags, 0, header);
    bytes.extend(record(
        b"GMST",
        0,
        ((masters.len() as u32) << 24) | 0x800,
        [
            subrecord(b"EDID", format!("{editor}\0").as_bytes()),
            subrecord(b"DATA", &67.0f32.to_le_bytes()),
        ]
        .concat(),
    ));
    bytes
}

/// Allow an explicitly chosen case-sensitive volume without copying checkout or game data.
fn fixture() -> tempfile::TempDir {
    match std::env::var_os("MUDCRAB_CASE_SENSITIVE_TEST_ROOT") {
        Some(root) => tempfile::Builder::new()
            .prefix("mudcrab-plugin-collisions-")
            .tempdir_in(root)
            .expect("MUDCRAB_CASE_SENSITIVE_TEST_ROOT must be an existing writable directory"),
        None => tempfile::tempdir().unwrap(),
    }
}

/// Refuse an unsupported volume explicitly instead of counting a skipped assertion as a pass.
fn write_distinct(data: &Path, name: &str, bytes: &[u8]) {
    let path = data.join(name);
    let mut file = fs::File::options()
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap_or_else(|error| {
            panic!(
                "case-sensitive filesystem required for distinct plugin {name:?}: {error}; \
                 set MUDCRAB_CASE_SENSITIVE_TEST_ROOT to a case-sensitive volume"
            )
        });
    file.write_all(bytes).unwrap();
}

/// Check actual names, metadata and bytes before using the fixture to prove selection behavior.
fn collision_pair(data: &Path, first: &str, second: &str) {
    let first_bytes = plugin_bytes(0, &[], "fCollisionFirst");
    let second_bytes = plugin_bytes(0x200, &[], "fCollisionSecond");
    write_distinct(data, first, &first_bytes);
    write_distinct(data, second, &second_bytes);
    let names: Vec<_> = fs::read_dir(data)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(names.contains(&first.into()) && names.contains(&second.into()));
    assert_eq!(fs::read(data.join(first)).unwrap(), first_bytes);
    assert_eq!(fs::read(data.join(second)).unwrap(), second_bytes);
    assert_eq!(parse_plugin_metadata(&data.join(first)).unwrap().flags, 0);
    assert_eq!(
        parse_plugin_metadata(&data.join(second)).unwrap().flags,
        0x200
    );
}

/// Read the public production selection path and inspect its independently encoded survivors.
fn selected_names(root: &Path, list: &[u8], ccc: &[u8], ini: &[u8]) -> Vec<String> {
    fs::write(root.join("plugins.txt"), list).unwrap();
    fs::write(root.join("Skyrim.ccc"), ccc).unwrap();
    fs::write(root.join("Skyrim.ini"), ini).unwrap();
    let paths = read_plugins_txt_with_options(
        &root.join("plugins.txt"),
        &root.join("Data"),
        Some(&root.join("Skyrim.ccc")),
        Some(&root.join("Skyrim.ini")),
    )
    .unwrap();
    let records = EsmParser::merge_plugins(&paths).unwrap();
    assert!(
        records
            .values()
            .all(|record| record.subrecords.iter().all(|(_, data)| !data
                .windows(b"fCollision".len())
                .any(|window| window == b"fCollision")))
    );
    paths
        .iter()
        .map(|path| path.file_name().unwrap().to_str().unwrap().to_owned())
        .collect()
}

/// Both usable ASCII variants and their transitive dependents are excluded, preserving list order.
#[test]
#[cfg_attr(
    any(target_os = "macos", target_os = "windows"),
    ignore = "requires case-sensitive filesystem; set MUDCRAB_CASE_SENSITIVE_TEST_ROOT and run --ignored"
)]
fn case_sensitive_ascii_collision_omits_both_files_and_dependents() {
    let dir = fixture();
    let data = dir.path().join("Data");
    fs::create_dir(&data).unwrap();
    collision_pair(&data, "Mod.esp", "mod.esp");
    for (name, masters, editor) in [
        ("Skyrim.esm", vec![], "fBase"),
        ("Good.esp", vec![], "fGood"),
        ("Child.esp", vec![b"Mod.esp".as_slice()], "fChild"),
        (
            "Grandchild.esp",
            vec![b"Child.esp".as_slice()],
            "fGrandchild",
        ),
        ("Later.esp", vec![], "fLater"),
    ] {
        write_distinct(&data, name, &plugin_bytes(0, &masters, editor));
    }
    assert_eq!(
        selected_names(
            dir.path(),
            b"*Good.esp\n*Mod.esp\n*Child.esp\n*Grandchild.esp\n*Later.esp\n",
            b"",
            b""
        ),
        ["Skyrim.esm", "Good.esp", "Later.esp"]
    );
}

/// Unicode lowercase collisions use the same exclusion policy and Windows-1252 master identity.
#[test]
#[cfg_attr(
    any(target_os = "macos", target_os = "windows"),
    ignore = "requires case-sensitive filesystem; set MUDCRAB_CASE_SENSITIVE_TEST_ROOT and run --ignored"
)]
fn case_sensitive_unicode_collision_omits_both_files_and_dependents() {
    let dir = fixture();
    let data = dir.path().join("Data");
    fs::create_dir(&data).unwrap();
    collision_pair(&data, "CAFÉ.esp", "café.esp");
    write_distinct(&data, "Good.esp", &plugin_bytes(0, &[], "fGood"));
    write_distinct(
        &data,
        "Child.esp",
        &plugin_bytes(0, &[b"CAF\xc9.esp"], "fChild"),
    );
    assert_eq!(
        selected_names(
            dir.path(),
            b"*CAF\xc9.esp\n*Child.esp\n*Good.esp\n",
            b"",
            b""
        ),
        ["Good.esp"]
    );
}

/// Official, CCC and explicit INI activation cannot choose exact-spelling winners for collisions.
#[test]
#[cfg_attr(
    any(target_os = "macos", target_os = "windows"),
    ignore = "requires case-sensitive filesystem; set MUDCRAB_CASE_SENSITIVE_TEST_ROOT and run --ignored"
)]
fn case_sensitive_implicit_collisions_omit_official_ccc_and_ini_dependents() {
    let dir = fixture();
    let data = dir.path().join("Data");
    fs::create_dir(&data).unwrap();
    for (first, second) in [
        ("Skyrim.esm", "skyrim.esm"),
        ("Creation.esp", "creation.esp"),
        ("Test.esp", "test.esp"),
    ] {
        collision_pair(&data, first, second);
    }
    write_distinct(&data, "Good.esp", &plugin_bytes(0, &[], "fGood"));
    for (name, master) in [
        ("OfficialChild.esp", b"Skyrim.esm".as_slice()),
        ("CreationChild.esp", b"Creation.esp".as_slice()),
        ("TestChild.esp", b"Test.esp".as_slice()),
    ] {
        write_distinct(&data, name, &plugin_bytes(0, &[master], "fChild"));
    }
    assert_eq!(
        selected_names(
            dir.path(),
            b"*OfficialChild.esp\n*Good.esp\n*CreationChild.esp\n*TestChild.esp\n",
            b"Creation.esp\n",
            b"[General]\nsTestFile1=Test.esp\n"
        ),
        ["Good.esp"]
    );
}

/// Automatic discovery excludes Unicode-normalized filename collisions and preserves usable records.
#[tokio::test]
#[cfg_attr(
    target_os = "macos",
    ignore = "requires a filesystem preserving distinct K and Kelvin-sign filenames"
)]
async fn automatic_unicode_collision_preserves_neighbor_publication() {
    for reverse_creation in [false, true] {
        let dir = fixture();
        let data = dir.path().join("Data");
        dummy_content::layout::prepare_directory(&data, false).unwrap();
        dummy_content::layout::generate(
            &data,
            dummy_content::layout::DEFAULT_SEED,
            dummy_content::layout::Formats::all(),
        )
        .unwrap();
        let first = plugin_bytes(0, &[], "fCollisionFirst");
        let second = plugin_bytes(0x200, &[], "fCollisionSecond");
        let colliders = if reverse_creation {
            [("\u{212A}.esp", &second), ("K.esp", &first)]
        } else {
            [("K.esp", &first), ("\u{212A}.esp", &second)]
        };
        for (name, bytes) in colliders {
            write_distinct(&data, name, bytes);
        }
        assert_eq!(fs::read(data.join("K.esp")).unwrap(), first);
        assert_eq!(fs::read(data.join("\u{212A}.esp")).unwrap(), second);
        assert_eq!(parse_plugin_metadata(&data.join("K.esp")).unwrap().flags, 0);
        assert_eq!(
            parse_plugin_metadata(&data.join("\u{212A}.esp"))
                .unwrap()
                .flags,
            0x200
        );
        for (name, master, editor) in [
            ("Good.esp", b"Skyrim.esm".as_slice(), "fJumpHeightMin"),
            ("Child.esp", b"K.esp".as_slice(), "fCollisionChild"),
            (
                "Grandchild.esp",
                b"Child.esp".as_slice(),
                "fCollisionGrandchild",
            ),
        ] {
            write_distinct(&data, name, &plugin_bytes(0, &[master], editor));
        }
        let output = dir.path().join("pack");
        let mut config = converter::PipelineConfig::new(&data, &output);
        config.no_lod = true;
        config.cpu_jobs = 2;
        config.io_jobs = 1;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<converter::ProgressEvent>(128);
        let drain = tokio::spawn(async move {
            let mut notices = Vec::new();
            while let Some(event) = rx.recv().await {
                if event.notice {
                    notices.push(event.message);
                }
            }
            notices
        });
        let result = converter::AssetPipeline::run_async(config, tx).await;
        let progress_notices = drain.await.unwrap();
        assert!(
            result.is_ok(),
            "recoverable automatic collision must preserve publication: {result:?}"
        );
        let report = result.unwrap();
        assert!(report.complete, "{report:?}");
        assert_eq!(report.skipped, 0, "{report:?}");
        assert!(output.join("conversion-manifest.json").is_file());
        assert!(output.join("meshes/generated.glb").is_file());
        let conn = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
        converter::esm::exporter::validate_database(&conn).unwrap();
        let names: Vec<String> = conn
            .prepare("SELECT name FROM plugins ORDER BY priority")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(names, ["Skyrim.esm", "Good.esp"]);
        let good: f64 = conn
            .query_row(
                "SELECT value FROM movement_game_settings WHERE editor_id='fJumpHeightMin'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(good, 67.0);
        let payloads: Vec<Vec<u8>> = conn
            .prepare("SELECT data FROM records WHERE record_type='GMST'")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let editors: Vec<_> = payloads
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
        assert!(editors.iter().any(|editor| editor == "fJumpHeightMin"));
        assert!(
            !editors
                .iter()
                .any(|editor| editor.starts_with("fCollision"))
        );
        let collision = "installed plugin name collision for k.esp; skipping all matching files";
        assert_eq!(
            report
                .notices
                .iter()
                .filter(|notice| notice.as_str() == collision)
                .count(),
            1,
            "{report:?}"
        );
        let collision_progress = format!("warning: {collision}");
        assert_eq!(
            progress_notices
                .iter()
                .filter(|notice| notice.as_str() == collision_progress.as_str())
                .count(),
            1,
            "{progress_notices:?}"
        );
        for name in ["Child.esp", "Grandchild.esp"] {
            assert!(
                report
                    .notices
                    .iter()
                    .any(|notice| notice.starts_with(&format!("skipping {name}:"))
                        && notice.contains("K.esp")),
                "{report:?}"
            );
        }
    }
}
