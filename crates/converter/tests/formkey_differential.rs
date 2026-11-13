//! Independent legacy packing versus the Mutagen-inspired FormKey contract.
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

// Frozen old permissive header packer: source selectors address MAST, then self.
// This oracle never calls resolve, FormKey, LegacyIdView, or converter remapping.
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

fn compare(plugins: &[Plugin], records: &[(usize, u32, u32)]) -> Vec<u8> {
    let t = table(plugins);
    let order: Vec<_> = plugins.iter().map(|p| t.id(&p.name).unwrap()).collect();
    let view = LegacyIdView::from_load_order(&t, &order).unwrap();
    let mut old = BTreeMap::new();
    let mut new = BTreeMap::<FormKey, u32>::new();
    for &(supplier, raw, payload) in records {
        let expected = old_pack(plugins, supplier, raw).unwrap();
        let resolved = key(&t, &plugins[supplier], raw).key();
        let actual = resolved.map_or(0, |k| view.to_legacy_u32(k).unwrap());
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
    // Step 1 explicitly projects past-MAST headers; step 2 must decide whether to
    // reject this outcome at the converter seam, without changing the pure resolver.
    assert!(matches!(
        key(&t, &plugins[1], 0x7f000801),
        Resolution::SelfProjected { .. }
    ));
    assert!(key(&t, &plugins[0], 0x801).key().is_some());
    assert!(key(&t, &plugins[1], 0x01000802).key().is_some());
}

#[test]
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

#[test]
#[ignore = "read-only 80-plugin audit; set FORMKEY_REAL80_AUDIT=1 explicitly"]
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
