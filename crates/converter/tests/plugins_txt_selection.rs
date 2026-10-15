use converter::esm::{load_order::LoadOrder, read_plugins_txt_with_options};
use serde::Deserialize;
use std::{
    collections::HashMap,
    fs,
    path::Path,
    time::{Duration, UNIX_EPOCH},
};

#[derive(Deserialize)]
struct Fixtures {
    cases: Vec<Case>,
    blobs: HashMap<String, Vec<u8>>,
}

#[derive(Deserialize)]
struct Case {
    case: String,
    files: Vec<Plugin>,
    list: Vec<u8>,
    ccc: Vec<u8>,
    ini: Vec<u8>,
    names: Vec<String>,
    normal: HashMap<String, u32>,
    light: HashMap<String, u32>,
}

#[derive(Deserialize)]
struct Plugin {
    name: String,
    blob: String,
    mtime: u64,
}

/// Materialize only synthetic answer-key data, setting times independently of checkout.
fn materialize(root: &Path, case: &Case, blobs: &HashMap<String, Vec<u8>>) {
    fs::create_dir(root.join("Data")).unwrap();
    for plugin in &case.files {
        let path = root.join("Data").join(&plugin.name);
        fs::write(&path, &blobs[&plugin.blob]).unwrap();
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(
                fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(plugin.mtime)),
            )
            .unwrap();
    }
    fs::write(root.join("plugins.txt"), &case.list).unwrap();
    fs::write(root.join("Skyrim.ccc"), &case.ccc).unwrap();
    fs::write(root.join("Skyrim.ini"), &case.ini).unwrap();
}

/// Match independently supplied final order and both slot classes for one fixture.
fn assert_case(name: &str) {
    let fixtures: Fixtures =
        serde_json::from_str(include_str!("fixtures/plugins_txt/cases.json")).unwrap();
    assert_eq!(fixtures.cases.len(), 10);
    let case = fixtures
        .cases
        .iter()
        .find(|case| case.case == name)
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    materialize(dir.path(), case, &fixtures.blobs);
    let result = read_plugins_txt_with_options(
        &dir.path().join("plugins.txt"),
        &dir.path().join("Data"),
        Some(&dir.path().join("Skyrim.ccc")),
        Some(&dir.path().join("Skyrim.ini")),
    );
    assert!(result.is_ok(), "{} selection failed: {result:?}", case.case);
    let paths = result.unwrap();
    let names: Vec<_> = paths
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().to_lowercase())
        .collect();
    assert_eq!(names, case.names, "{} selected order", case.case);
    let result = LoadOrder::read(&paths);
    assert!(result.is_ok(), "{} slot assignment failed", case.case);
    let order = result.unwrap();
    assert_eq!(order.normal, case.normal, "{} full slots", case.case);
    assert_eq!(order.light, case.light, "{} light slots", case.case);
}

macro_rules! oracle_case {
    ($name:ident) => {
        #[doc = "Compare synthetic selection and slots against the supplied black-box answer key."]
        #[test]
        fn $name() {
            assert_case(stringify!($name));
        }
    };
}
oracle_case!(empty_player_list);
oracle_case!(cc_missing_and_fixed);
oracle_case!(inactive_and_duplicates);
oracle_case!(duplicate_opposite_activation_last_wins);
oracle_case!(ini_unlisted_timestamp_order);
oracle_case!(windows_1252);
oracle_case!(windows_1252_master_dependency);
oracle_case!(master_and_light_flags);
oracle_case!(regular_dependency_hoist);
oracle_case!(ini_preserves_listed_position);

/// Resolve the default CCC beside Data and tolerate both absent and unreadable optional files.
#[test]
fn default_ccc_and_optional_list_errors_preserve_selection() {
    let fixtures: Fixtures =
        serde_json::from_str(include_str!("fixtures/plugins_txt/cases.json")).unwrap();
    let case = fixtures
        .cases
        .iter()
        .find(|case| case.case == "cc_missing_and_fixed")
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    materialize(dir.path(), case, &fixtures.blobs);
    let list = dir.path().join("plugins.txt");
    let data = dir.path().join("Data");
    let paths = read_plugins_txt_with_options(&list, &data, None, None).unwrap();
    assert_eq!(LoadOrder::read(&paths).unwrap().names, case.names);
    for unavailable in [dir.path().join("missing"), dir.path().join("Data")] {
        let paths =
            read_plugins_txt_with_options(&list, &data, Some(&unavailable), Some(&unavailable))
                .unwrap();
        let order = LoadOrder::read(&paths).unwrap();
        assert_eq!(&order.names[..5], &case.names[..5]);
        assert!(order.names.contains(&"regular.esp".to_owned()));
        assert!(!order.names.contains(&"test.esp".to_owned()));
    }
}

/// Comment-only real selection must reproduce the baseline explicit list and merge count.
#[test]
#[ignore = "requires explicit MUDCRAB_GRASS_DATA, MUDCRAB_SKYRIM_CCC and MUDCRAB_ORDER_BASELINE"]
fn official_comment_only_list_matches_main() {
    let data = std::path::PathBuf::from(std::env::var_os("MUDCRAB_GRASS_DATA").unwrap());
    let ccc = std::path::PathBuf::from(std::env::var_os("MUDCRAB_SKYRIM_CCC").unwrap());
    let baseline = std::path::PathBuf::from(std::env::var_os("MUDCRAB_ORDER_BASELINE").unwrap());
    let expected: serde_json::Value = serde_json::from_slice(&fs::read(baseline).unwrap()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let list = dir.path().join("plugins.txt");
    fs::write(&list,"# This file is used by Skyrim to keep track of your downloaded content.\n# Please do not modify this file.\n").unwrap();
    let paths = read_plugins_txt_with_options(&list, &data, Some(&ccc), None).unwrap();
    let order = LoadOrder::read(&paths).unwrap();
    assert_eq!(
        serde_json::json!({"names":order.names,"normal":order.normal,"light":order.light}),
        expected
    );
    assert_eq!(paths.len(), 80);
    let records = converter::esm::EsmParser::merge_plugins(&paths).unwrap();
    assert_eq!(records.len(), 1_240_167);
    eprintln!(
        "Comment-only list + real Skyrim.ccc: {} plugins / {} effective records; exact baseline order and slots match",
        paths.len(),
        records.len()
    );
}
