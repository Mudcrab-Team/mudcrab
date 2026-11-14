//! Selection must finish before every downstream consumer assigns supplier slots.
use super::*;

fn plugin(path: &Path, masters: &[&str], id: u32) {
    let record = |tag: &[u8; 4], id: u32, body: Vec<u8>| {
        [
            tag.as_slice(),
            &(body.len() as u32).to_le_bytes(),
            &[0; 4],
            &id.to_le_bytes(),
            &[0; 8],
            &body,
        ]
        .concat()
    };
    let field = |tag: &[u8; 4], bytes: &[u8]| {
        [tag.as_slice(), &(bytes.len() as u16).to_le_bytes(), bytes].concat()
    };
    let header = masters
        .iter()
        .flat_map(|name| {
            [
                field(b"MAST", format!("{name}\0").as_bytes()),
                field(b"DATA", &[0; 8]),
            ]
            .concat()
        })
        .collect();
    fs::write(
        path,
        [
            record(b"TES4", 0, header),
            record(b"LTEX", id, field(b"EDID", b"Texture\0")),
        ]
        .concat(),
    )
    .unwrap();
}

#[test]
fn whitespace_name_selection_aligns_archives_and_terrain_slots() {
    // Unicode trailing whitespace survives Windows filename handling and has an .esp extension.
    for invalid in [" Middle.esp", "Middle.esp\u{2003}"] {
        let root = tempfile::tempdir().unwrap();
        let config = PipelineConfig::new(root.path(), root.path().join("out"));
        let names = ["Base.esm", invalid, "Child.esp", "Later.esp", "Last.esp"];
        for (name, masters, id) in [
            (names[0], vec![], 0x811),
            (names[1], vec!["Base.esm"], 0x0100_0822),
            (names[2], vec![invalid], 0x0100_0833),
            (names[3], vec!["Base.esm"], 0x0100_0844),
            (names[4], vec!["Base.esm", "Later.esp"], 0x0200_0855),
        ] {
            plugin(&root.path().join(name), &masters, id);
        }
        let paths = names.map(|name| root.path().join(name));
        // Exercise discovery at the pipeline boundary and the explicit ordered-path entry point.
        for explicit in [false, true] {
            let mut notices = Vec::new();
            let mut skipped = Vec::new();
            let accepted = if explicit {
                crate::esm::plugins_txt::recover_with_diagnostics(
                    paths.to_vec(),
                    &mut notices,
                    &mut skipped,
                )
                .unwrap()
            } else {
                plugin_paths_with_diagnostics(
                    &config,
                    &paths,
                    &mut notices,
                    &mut skipped,
                    &mut Vec::new(),
                )
                .unwrap()
            };
            let accepted_names: Vec<_> = accepted
                .iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect();
            assert_eq!(
                accepted_names,
                ["Base.esm", "Later.esp", "Last.esp"],
                "{notices:?}"
            );
            assert!(skipped.contains(&invalid.to_owned()));
            assert!(skipped.contains(&"Child.esp".to_owned()));
            let order = crate::esm::load_order::LoadOrder::read(&accepted).unwrap();
            let masters = crate::terrain_pipeline::load_order_masters(&order);
            let merged = crate::esm::EsmParser::merge_plugins(&accepted).unwrap();
            assert_eq!(merged[&0x0100_0844].load_order, 1);
            assert_eq!(merged[&0x0200_0855].load_order, 2);
            assert_eq!(masters[&merged[&0x0100_0844].load_order], ["Base.esm"]);
            assert_eq!(
                masters[&merged[&0x0200_0855].load_order],
                ["Base.esm", "Later.esp"]
            );
            assert_eq!(
                archive_load_order_priority(&root.path().join("Later.bsa"), &accepted),
                Some(1)
            );
            assert_eq!(
                archive_load_order_priority(&root.path().join("Last.bsa"), &accepted),
                Some(2)
            );
            assert_eq!(
                archive_load_order_priority(&root.path().join("Child.bsa"), &accepted),
                None
            );
        }
    }
}
