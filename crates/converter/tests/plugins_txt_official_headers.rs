//! Optional read-only official selection/slot checks; plugin records are never decoded.

use converter::esm::{load_order::LoadOrder, read_plugins_txt_with_options};
use serde::Deserialize;
use std::{collections::HashMap, fs, path::PathBuf};

#[derive(Deserialize)]
struct Baseline {
    player: Order,
}

#[derive(Deserialize)]
struct Order {
    names: Vec<String>,
    original_names: Vec<String>,
    normal: HashMap<String, u32>,
    light: HashMap<String, u32>,
}

/// Require explicitly selected source paths and a retained independent output baseline.
fn environment() -> (PathBuf, PathBuf, Baseline) {
    let data = PathBuf::from(std::env::var_os("MUDCRAB_GRASS_DATA").unwrap());
    let ccc = PathBuf::from(std::env::var_os("MUDCRAB_SKYRIM_CCC").unwrap());
    let baseline = PathBuf::from(std::env::var_os("MUDCRAB_ORDER_BASELINE").unwrap());
    let baseline = serde_json::from_slice(&fs::read(baseline).unwrap()).unwrap();
    (data, ccc, baseline)
}

/// Compare all selected filenames and both header-derived slot maps, without record decoding.
fn assert_order(paths: &[PathBuf], expected: &Order) {
    assert_eq!(paths.len(), 80);
    let original_names: Vec<_> = paths
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(original_names, expected.original_names);
    let order = LoadOrder::read(paths).unwrap();
    assert_eq!(order.names, expected.names);
    assert_eq!(order.normal.len(), 15);
    assert_eq!(order.light.len(), 65);
    assert_eq!(order.normal, expected.normal);
    assert_eq!(order.light, expected.light);
    eprintln!(
        "Read-only official headers: exact 80 names, 15 full slots and 65 light slots match baseline"
    );
}

/// Comment-only player selection plus the readable CCC must preserve the official order/slots.
#[test]
#[ignore = "requires explicit MUDCRAB_GRASS_DATA, MUDCRAB_SKYRIM_CCC and MUDCRAB_ORDER_BASELINE"]
fn official_comment_only_player_selection_matches_header_baseline() {
    let (data, ccc, baseline) = environment();
    let directory = tempfile::tempdir().unwrap();
    let list = directory.path().join("plugins.txt");
    fs::write(&list, "# Header-only official player selection audit\n").unwrap();
    let paths = read_plugins_txt_with_options(&list, &data, Some(&ccc), None).unwrap();
    assert_order(&paths, &baseline.player);
}
