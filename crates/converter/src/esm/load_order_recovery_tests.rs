//! Synthetic TES4 headers exercise admission without sharing the ordering algorithm.
use super::{LoadOrder, order_explicit_plugins_with_notices, recover_ordered_plugins_with_notices};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Encode just the header needed for dependency and slot admission.
fn plugin(root: &Path, name: &str, masters: &[&str], flags: u32) -> PathBuf {
    let mut payload = Vec::new();
    for master in masters {
        payload.extend(b"MAST");
        payload.extend(((master.len() + 1) as u16).to_le_bytes());
        payload.extend(master.as_bytes());
        payload.push(0);
        payload.extend(b"DATA");
        payload.extend(8u16.to_le_bytes());
        payload.extend([0; 8]);
    }
    let mut bytes = Vec::from(b"TES4");
    bytes.extend((payload.len() as u32).to_le_bytes());
    bytes.extend(flags.to_le_bytes());
    bytes.extend([0; 12]);
    bytes.extend(payload);
    let path = root.join(name);
    fs::write(&path, bytes).unwrap();
    path
}

/// Recovery removes the actual three-node cycle and its dependent closure,
/// preserving the existing master hoist and nonsorted neighboring plugin order.
#[test]
fn three_plugin_cycle_skips_dependents_and_preserves_neighbor_order_and_slots() {
    let dir = tempfile::tempdir().unwrap();
    let paths = vec![
        plugin(dir.path(), "ZNeighbor.esp", &[], 0),
        plugin(dir.path(), "CycleA.esp", &["CycleB.esm"], 0),
        plugin(dir.path(), "CycleB.esm", &["CycleC.esp"], 1),
        plugin(dir.path(), "CycleC.esp", &["CycleA.esp"], 0),
        plugin(dir.path(), "Tail.esp", &["CycleC.esp"], 0),
        plugin(dir.path(), "Deep.esp", &["Tail.esp"], 0),
        plugin(dir.path(), "Base.esm", &[], 1),
        plugin(dir.path(), "AMaster.esm", &["Required.esp"], 1),
        plugin(dir.path(), "Required.esp", &["Base.esm"], 0),
        plugin(dir.path(), "LightNeighbor.esp", &[], 0x200),
        plugin(dir.path(), "ANeighbor.esp", &["Base.esm"], 0),
    ];
    let mut notices = Vec::new();
    let mut skipped = Vec::new();
    let ordered = order_explicit_plugins_with_notices(paths, &mut notices, &mut skipped).unwrap();
    let order = LoadOrder::read(&ordered);
    assert!(order.is_ok(), "cycle recovery must retain usable neighbors");
    let order = order.unwrap();
    assert_eq!(
        order.names,
        [
            "base.esm",
            "required.esp",
            "amaster.esm",
            "zneighbor.esp",
            "lightneighbor.esp",
            "aneighbor.esp",
        ]
    );
    for (name, slot) in [
        ("base.esm", 0),
        ("required.esp", 1),
        ("amaster.esm", 2),
        ("zneighbor.esp", 3),
        ("aneighbor.esp", 4),
    ] {
        assert_eq!(order.normal[name], slot, "{name}");
    }
    assert_eq!(order.light["lightneighbor.esp"], 0);
    assert_eq!(
        skipped,
        [
            "CycleA.esp",
            "CycleB.esm",
            "CycleC.esp",
            "Tail.esp",
            "Deep.esp"
        ]
    );
    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains("cyclic plugin dependencies"));
    assert!(notices[0].contains("3 cycle members"));
    assert!(notices[0].contains("2 dependent plugins"));
    for member in ["CycleA.esp", "CycleB.esm", "CycleC.esp"] {
        assert!(notices[0].contains(member));
    }
    assert!(notices[0].len() < 2048);
}

/// Two excess full plugins cannot displace admitted full plugins or retain
/// light dependents; independent light neighbors keep their original slots.
#[test]
fn full_slot_overflow_keeps_first_254_and_skips_transitive_light_dependents() {
    let dir = tempfile::tempdir().unwrap();
    let mut paths: Vec<_> = (0..256)
        .map(|index| plugin(dir.path(), &format!("Full{index:03}.esp"), &[], 0))
        .collect();
    paths.extend([
        plugin(dir.path(), "LightTail.esp", &["Full254.esp"], 0x200),
        plugin(dir.path(), "LightDeep.esp", &["LightTail.esp"], 0x200),
        plugin(dir.path(), "Keep.esp", &[], 0x200),
    ]);
    let mut notices = Vec::new();
    let mut skipped = Vec::new();
    let ordered = order_explicit_plugins_with_notices(paths, &mut notices, &mut skipped).unwrap();
    let order = LoadOrder::read(&ordered);
    assert!(
        order.is_ok(),
        "full overflow must preserve admitted neighbors"
    );
    let order = order.unwrap();
    let expected: Vec<_> = (0..254)
        .map(|index| format!("full{index:03}.esp"))
        .chain(std::iter::once("keep.esp".to_owned()))
        .collect();
    assert_eq!(order.names, expected);
    assert_eq!(order.normal.len(), 254);
    assert_eq!(order.light.len(), 1);
    for index in 0..254 {
        assert_eq!(order.normal[&format!("full{index:03}.esp")], index);
    }
    assert_eq!(order.light["keep.esp"], 0);
    assert_eq!(
        skipped,
        [
            "Full254.esp",
            "Full255.esp",
            "LightTail.esp",
            "LightDeep.esp"
        ]
    );
    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains("full plugin slot overflow"));
    assert!(notices[0].contains("skipped 2 full plugins"));
    assert!(notices[0].contains("first: Full254.esp"));
    assert!(notices[0].contains("2 dependent plugins"));
    assert!(notices[0].len() < 2048);
}

/// One excess light plugin and its full dependent closure cannot prevent the
/// 4096 admitted light plugins and an independent full neighbor from loading.
#[test]
fn light_slot_overflow_keeps_first_4096_and_skips_transitive_full_dependents() {
    let dir = tempfile::tempdir().unwrap();
    let mut paths: Vec<_> = (0..4097)
        .map(|index| plugin(dir.path(), &format!("Light{index:04}.esl"), &[], 0x200))
        .collect();
    paths.extend([
        plugin(dir.path(), "FullTail.esp", &["Light4096.esl"], 0),
        plugin(dir.path(), "FullDeep.esp", &["FullTail.esp"], 0),
        plugin(dir.path(), "FullKeep.esp", &[], 0),
    ]);
    let mut notices = Vec::new();
    let mut skipped = Vec::new();
    let ordered = order_explicit_plugins_with_notices(paths, &mut notices, &mut skipped).unwrap();
    let order = LoadOrder::read(&ordered);
    assert!(
        order.is_ok(),
        "light overflow must preserve admitted neighbors"
    );
    let order = order.unwrap();
    let expected: Vec<_> = (0..4096)
        .map(|index| format!("light{index:04}.esl"))
        .chain(std::iter::once("fullkeep.esp".to_owned()))
        .collect();
    assert_eq!(order.names, expected);
    assert_eq!(order.light.len(), 4096);
    assert_eq!(order.normal.len(), 1);
    for index in 0..4096 {
        assert_eq!(order.light[&format!("light{index:04}.esl")], index);
    }
    assert_eq!(order.normal["fullkeep.esp"], 0);
    assert_eq!(skipped, ["Light4096.esl", "FullTail.esp", "FullDeep.esp"]);
    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains("light plugin slot overflow"));
    assert!(notices[0].contains("skipped 1 light plugins"));
    assert!(notices[0].contains("first: Light4096.esl"));
    assert!(notices[0].contains("2 dependent plugins"));
    assert!(notices[0].len() < 2048);
}

/// Large cyclic closures and self-cycles retain complete structured diagnostics
/// while warning length and warning count stay independent of closure size.
#[test]
fn cyclic_warning_is_bounded_and_diagnostics_include_every_skipped_plugin() {
    let dir = tempfile::tempdir().unwrap();
    let mut paths = Vec::new();
    let mut expected_skipped = Vec::new();
    for index in 0..100 {
        let name = format!("Cycle{index:03}.esp");
        let master = format!("Cycle{:03}.esp", (index + 1) % 100);
        paths.push(plugin(dir.path(), &name, &[&master], 0));
        expected_skipped.push(name);
    }
    for index in 0..200 {
        let name = format!("Dependent{index:03}.esp");
        let master = if index == 0 {
            "Cycle000.esp".to_owned()
        } else {
            format!("Dependent{:03}.esp", index - 1)
        };
        paths.push(plugin(dir.path(), &name, &[&master], 0));
        expected_skipped.push(name);
    }
    paths.push(plugin(dir.path(), "Self.esp", &["Self.esp"], 0));
    expected_skipped.push("Self.esp".to_owned());
    paths.push(plugin(dir.path(), "Neighbor.esp", &[], 0));
    let mut notices = Vec::new();
    let mut skipped = Vec::new();
    let ordered = recover_ordered_plugins_with_notices(paths, &mut notices, &mut skipped).unwrap();
    let order = LoadOrder::read(&ordered).unwrap();
    assert_eq!(order.names, ["neighbor.esp"]);
    assert_eq!(order.normal["neighbor.esp"], 0);
    assert_eq!(skipped, expected_skipped);
    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains("101 cycle members"));
    assert!(notices[0].contains("200 dependent plugins"));
    assert!(notices[0].contains("and 97 more"));
    assert!(notices[0].len() < 2048);
}

/// Admission follows the existing master hoist rather than the original unsorted
/// input prefix, preserving the surviving dependencies and existing slot rules.
#[test]
fn full_slot_prefix_is_measured_after_existing_master_dependency_hoist() {
    let dir = tempfile::tempdir().unwrap();
    let mut paths: Vec<_> = (0..256)
        .map(|index| plugin(dir.path(), &format!("Full{index:03}.esp"), &[], 0))
        .collect();
    paths.push(plugin(dir.path(), "Master.esl", &["Full255.esp"], 0x200));
    let mut notices = Vec::new();
    let mut skipped = Vec::new();
    let ordered = order_explicit_plugins_with_notices(paths, &mut notices, &mut skipped).unwrap();
    let order = LoadOrder::read(&ordered).unwrap();
    let expected: Vec<_> = ["full255.esp".to_owned(), "master.esl".to_owned()]
        .into_iter()
        .chain((0..253).map(|index| format!("full{index:03}.esp")))
        .collect();
    assert_eq!(order.names, expected);
    assert_eq!(order.normal["full255.esp"], 0);
    for index in 0..253 {
        assert_eq!(order.normal[&format!("full{index:03}.esp")], index + 1);
    }
    assert_eq!(order.light["master.esl"], 0);
    assert_eq!(skipped, ["Full253.esp", "Full254.esp"]);
    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains("first: Full253.esp"));
}

/// A regular plugin listed before its regular master is skipped with its dependents rather
/// than moved; neighbors keep their listed order and slots in explicit and profile ordering.
#[test]
fn plugin_listed_before_its_master_is_skipped_with_dependents_not_reordered() {
    for profile in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let paths = vec![
            plugin(dir.path(), "Base.esm", &[], 1),
            plugin(dir.path(), "Patch.esp", &["Base.esm", "ZMod.esp"], 0),
            plugin(dir.path(), "PatchAddon.esp", &["Patch.esp"], 0x200),
            plugin(dir.path(), "Neighbor.esp", &["Base.esm"], 0),
            plugin(dir.path(), "ZMod.esp", &["Base.esm"], 0),
        ];
        let mut notices = Vec::new();
        let mut skipped = Vec::new();
        let ordered = if profile {
            recover_ordered_plugins_with_notices(paths, &mut notices, &mut skipped)
        } else {
            order_explicit_plugins_with_notices(paths, &mut notices, &mut skipped)
        }
        .unwrap();
        let order = LoadOrder::read(&ordered);
        assert!(
            order.is_ok(),
            "inversion recovery must keep usable neighbors (profile: {profile})"
        );
        let order = order.unwrap();
        assert_eq!(order.names, ["base.esm", "neighbor.esp", "zmod.esp"]);
        assert_eq!(order.normal["neighbor.esp"], 1);
        assert_eq!(order.normal["zmod.esp"], 2);
        assert_eq!(skipped, ["Patch.esp", "PatchAddon.esp"]);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].contains("plugins listed before their masters"));
        assert!(notices[0].contains("first: Patch.esp before ZMod.esp"));
        assert!(notices[0].contains("skipped 1 dependent plugins (first: PatchAddon.esp)"));
    }
}
