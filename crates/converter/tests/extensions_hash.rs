//! The manifest's extension hashes on real converter output: the Rust writer and the Python
//! test reference (`tests/support/extension_hash_reference.py`) must agree.

use dummy_content::layout;
use std::{fs, path::Path, process::Command};

fn generate_data(root: &Path) {
    layout::prepare_directory(root, false).unwrap();
    let formats = layout::Formats::parse("dds,pex,nif,esm").unwrap();
    layout::generate(root, layout::DEFAULT_SEED, formats).unwrap();
}

async fn convert(config: converter::PipelineConfig) -> converter::pipeline::PipelineReport {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = converter::AssetPipeline::run_async(config, tx)
        .await
        .unwrap();
    drain.await.unwrap();
    report
}

fn python() -> Option<&'static str> {
    ["python", "python3"].into_iter().find(|name| {
        Command::new(name)
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    })
}

#[tokio::test]
async fn python_reference_accepts_the_converter_manifest() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("assets");
    generate_data(&data);
    let report = convert(converter::PipelineConfig::new(&data, &output)).await;
    assert!(report.complete, "{:?}", report.warnings);

    let manifest: converter::cache::ConversionManifest =
        serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap())
            .unwrap();
    let ids: Vec<&str> = manifest
        .extensions
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    assert!(
        ids.is_empty(),
        "ordinary conversion must publish core output only: {ids:?}"
    );
    let json: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap())
            .unwrap();
    assert!(
        json.get("extensions").is_none(),
        "empty extensions are omitted"
    );
    let connection = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
    for table in [
        "actor_models",
        "actor_model_parts",
        "reference_linked_refs",
        "reference_activate_parents",
        "reference_details",
        "door_links",
        "emittance",
    ] {
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert!(exists, "missing current core table {table}");
    }
    let references: i64 = connection
        .query_row("SELECT COUNT(*) FROM \"references\"", [], |row| row.get(0))
        .unwrap();
    assert!(
        references > 0,
        "real conversion must retain core references"
    );
    drop(connection);
    assert!(
        !ids.contains(&shared::extensions::DOOR_LINKS),
        "door links are core output"
    );
    assert!(shared::extensions::ownership_problems(&manifest.extensions).is_empty());
    for entry in &manifest.extensions {
        assert_eq!(entry.sha256.len(), 64, "{}", entry.id);
    }

    // The Rust side recomputes the same hashes from the published folder.
    let again = converter::extensions::build_entries(&output).unwrap();
    assert_eq!(again, manifest.extensions);

    let Some(python) = python() else {
        eprintln!("skipped the Python cross check: `python` is not on PATH");
        return;
    };
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/extension_hash_reference.py");
    let result = Command::new(python)
        .arg("-I")
        .arg(&script)
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "tests/support/extension_hash_reference.py disagrees with the converter:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

/// Checks every extension a data set lists, own or foreign, under a shared asset lock.
///
/// * The converter's own extensions (`converter::extensions::specs`) are recomputed from the folder
///   and must equal the listed entries exactly: hash, identity, ownership and stats.
/// * Every other (finalized) entry goes through the composer, which requires the data set's
///   current non-empty core identity, unique ids, disjoint ownership (nested folders included) and
///   a `sha256` equal to `recompute_hash` of the content on disk. Nothing is minted or dropped.
///   The composer cannot recompute a foreign entry's `stats` or `version` meaning: those are
///   taken as listed and are not independently verified here.
/// * `tests/support/extension_hash_reference.py` must then accept every listed entry.
fn verify_set(set: &Path) {
    let _lock = shared::asset_lock::AssetLock::acquire_shared(set).unwrap();
    let manifest: converter::cache::ConversionManifest =
        serde_json::from_slice(&fs::read(set.join("conversion-manifest.json")).unwrap()).unwrap();
    let own_ids: Vec<String> = converter::extensions::specs()
        .iter()
        .map(|spec| spec.id.to_owned())
        .collect();
    let (own, foreign): (Vec<_>, Vec<_>) = manifest
        .extensions
        .iter()
        .cloned()
        .partition(|entry| own_ids.contains(&entry.id));
    assert_eq!(
        converter::extensions::build_entries(set).unwrap(),
        own,
        "Rust recompute of the converter's own extensions must match the listed entries"
    );
    let mut composed = converter::cache::ConversionManifest::default();
    converter::extensions::refresh_manifest_with_finalized(&mut composed, set, &foreign)
        .unwrap_or_else(|error| panic!("a listed foreign extension is not valid: {error:#}"));
    let by_id = |entries: &[shared::extensions::ExtensionEntry]| {
        let mut entries = entries.to_vec();
        entries.sort_by(|a, b| a.id.cmp(&b.id));
        entries
    };
    assert_eq!(
        by_id(&composed.extensions),
        by_id(&manifest.extensions),
        "the composed list must equal the manifest's, with nothing minted or dropped"
    );
    let python = python().expect("python on PATH");
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/extension_hash_reference.py");
    let result = Command::new(python)
        .arg("-I")
        .arg(&script)
        .arg(set)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
}

/// A finalized entry from another writer: its own table and
/// folder, built from the data set's core identity and listed next to the converter's entries.
#[tokio::test]
async fn a_finalized_foreign_entry_passes_and_a_tampered_one_fails() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("assets");
    generate_data(&data);
    convert(converter::PipelineConfig::new(&data, &output)).await;
    if python().is_none() {
        eprintln!("skipped: `python` is not on PATH");
        return;
    }
    let database = output.join("skyrim_world.db");
    let connection = rusqlite::Connection::open(&database).unwrap();
    let identity = shared::extensions::core_build_identity(&connection)
        .unwrap()
        .expect("the dummy conversion records a core build identity");
    connection
        .execute_batch(
            "CREATE TABLE extra_payload(a INTEGER); INSERT INTO extra_payload VALUES (7);",
        )
        .unwrap();
    drop(connection);
    fs::create_dir_all(output.join("payload")).unwrap();
    fs::write(output.join("payload/a.bin"), b"payload").unwrap();
    let mut entry = shared::extensions::ExtensionEntry {
        id: "example.payload".to_owned(),
        version: 1,
        owns: shared::extensions::ExtensionOwns {
            tables: vec!["extra_payload".to_owned()],
            files: vec!["payload/".to_owned()],
            ..Default::default()
        },
        sha256: String::new(),
        built_from: identity,
        stats: Default::default(),
    };
    entry.sha256 = converter::extensions::recompute_hash(&output, &entry).unwrap();
    let manifest_path = output.join("conversion-manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest
        .as_object_mut()
        .unwrap()
        .entry("extensions")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .unwrap()
        .push(serde_json::to_value(&entry).unwrap());
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    // The own-entries-only comparison this replaces would have failed here.
    verify_set(&output);

    fs::write(output.join("payload/a.bin"), b"tampered").unwrap();
    let outcome = std::panic::catch_unwind(|| verify_set(&output));
    assert!(outcome.is_err(), "changed owned bytes must be rejected");
}

#[test]
fn python_ownership_uses_sqlite_ascii_case_rules() {
    let python = python().expect("python on PATH");
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/extension_hash_reference.py");
    let code = r#"
import importlib.util, sqlite3, sys
spec = importlib.util.spec_from_file_location('oracle', sys.argv[1])
oracle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(oracle)
def entry(id, kind, name):
    return {'id': id, 'owns': {kind: [name]}}
connection = sqlite3.connect(':memory:')
for kind in ('tables', 'indexes'):
    for first, second in [('space_lighting', 'SPACE_LIGHTING'),
                          ('idx_lod_tree_block', 'IDX_LOD_TREE_BLOCK'),
                          ('Mixed_Ä', 'mIXED_Ä'), ('Ä', 'ä'), ('Σ', 'σ'), ('ß', 'ss')]:
        same = bool(connection.execute('SELECT ? = ? COLLATE NOCASE', (first, second)).fetchone()[0])
        a, b = entry('example.a', kind, first), entry('example.b', kind, second)
        assert bool(oracle.ownership_problems([a, b])) == same, (kind, first, second)
        assert bool(oracle.ownership_problems([b, a])) == same, (kind, first, second)
    for code in range(128):
        first, second = 'prefix_' + chr(code), 'prefix_' + chr(code).upper()
        same = bool(connection.execute('SELECT ? = ? COLLATE NOCASE', (first, second)).fetchone()[0])
        assert bool(oracle.ownership_problems([entry('a', kind, first), entry('b', kind, second)])) == same
for kind in ('files', 'gltf_extensions'):
    assert not oracle.ownership_problems([entry('a', kind, 'Payload'), entry('b', kind, 'payload')])
    assert oracle.ownership_problems([entry('a', kind, 'Payload'), entry('b', kind, 'Payload')])
connection.executescript('CREATE TABLE extra(a); CREATE INDEX idx_extra ON extra(a);')
assert oracle.index_line(connection, 'IDX_EXTRA') == 'index IDX_EXTRA CREATE INDEX idx_extra ON extra(a)'
"#;
    let result = Command::new(python)
        .arg("-B")
        .arg("-I")
        .arg("-c")
        .arg(code)
        .arg(script)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

/// A real data set, not a fixture: `MUDCRAB_EXTENSIONS_SET=<folder> cargo nextest run -p converter
/// --test extensions_hash --run-ignored all`. Every listed entry, the converter's own and any
/// finalized payload, is verified by [`verify_set`]; this is the check that
/// found a float-parsing disagreement between Rust and Python on real glTF data.
#[test]
#[ignore = "needs a converted data set: set MUDCRAB_EXTENSIONS_SET"]
fn real_data_set_hashes_agree_in_rust_and_python() {
    let Some(set) = std::env::var_os("MUDCRAB_EXTENSIONS_SET") else {
        panic!("set MUDCRAB_EXTENSIONS_SET to a converted data set folder");
    };
    verify_set(Path::new(&set));
}

#[test]
fn generic_gltf_locations_and_pointer_escaping_match_python() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let connection = rusqlite::Connection::open(root.join("skyrim_world.db")).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE lod_build(build_identity TEXT); INSERT INTO lod_build VALUES('core');",
        )
        .unwrap();
    drop(connection);
    fs::create_dir(root.join("meshes")).unwrap();
    let name = "EXAMPLE_payload~/";
    let document = serde_json::json!({
        "extensionsUsed": [name],
        "extensions": {name: {"value": 1}},
        "materials": [{"extensions": {name: {"value": 2}}}],
        "nodes": [{"extensions": {name: {"value": 3}}}],
        "meshes": [{"primitives": [{"extensions": {name: {"value": 4}}}]}],
        "extras": {"key~/": {"extensions": {name: {"value": 5}}}}
    });
    fs::write(
        root.join("meshes/example.gltf"),
        serde_json::to_vec(&document).unwrap(),
    )
    .unwrap();
    let mut entry = shared::extensions::ExtensionEntry {
        id: "example.payload".into(),
        version: 1,
        owns: shared::extensions::ExtensionOwns {
            gltf_extensions: vec![name.into()],
            ..Default::default()
        },
        sha256: String::new(),
        built_from: "core".into(),
        stats: Default::default(),
    };
    entry.sha256 = converter::extensions::recompute_hash(root, &entry).unwrap();
    let mut manifest = converter::cache::ConversionManifest::default();
    converter::extensions::refresh_manifest_with_finalized(&mut manifest, root, &[entry]).unwrap();
    manifest
        .save(&root.join("conversion-manifest.json"))
        .unwrap();
    verify_set(root);
}

#[test]
fn python_oracle_retains_the_pinned_hash_streams() {
    let python = python().expect("python on PATH");
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/extension_hash_reference.py");
    let code = r#"
import importlib.util, pathlib, sqlite3, sys, tempfile
spec = importlib.util.spec_from_file_location('oracle', sys.argv[1])
oracle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(oracle)
with tempfile.TemporaryDirectory() as root:
    root = pathlib.Path(root)
    db = sqlite3.connect(root / oracle.DATABASE)
    db.executescript('''
        CREATE TABLE statics (id INTEGER PRIMARY KEY, editor_id TEXT, model_path TEXT);
        INSERT INTO statics VALUES (16, 'RockA', 'meshes/rock.glb');
        CREATE TABLE door_links (ref_id INTEGER PRIMARY KEY, destination_ref_id INTEGER,
                                 pos_x REAL, pos_y REAL, pos_z REAL, note TEXT, data BLOB);
        INSERT INTO door_links VALUES (2, 1, 0.5, -1.25, 1e-7, 'Whiterun \u2713', x'00ff');
        INSERT INTO door_links VALUES (1, 2, 3.0, 4.0, NULL, NULL, NULL);
        CREATE INDEX idx_door_links_destination ON door_links(destination_ref_id);
    ''')
    db.close()
    entry = {'id': 'mudcrab.door_links', 'version': 1,
             'owns': {'tables': ['door_links'], 'indexes': ['idx_door_links_destination']}}
    assert oracle.extension_hash(root, entry) == '5c9f99438d7658c66aafebaa45045f6d553f0d52521309be28b1ef108db20e87'
    (root / 'meshes').mkdir()
    rock = {'asset': {'version': '2.0'}, 'extensionsUsed': ['MUDCRAB_material', 'OPEN_SKYRIM_material'],
            'materials': [{'extensions': {'OPEN_SKYRIM_material': {'shaderFlags1': 1},
                                          'MUDCRAB_material': {'uvScale': [1, 2.5], 'blendSource': 'SRC_ALPHA'}}},
                          {'extensions': {'OPEN_SKYRIM_material': {'shaderFlags1': 2}}}]}
    import json, struct
    payload = json.dumps(rock).encode('utf-8')
    payload += b' ' * (-len(payload) % 4)
    mesh = struct.pack('<III', oracle.GLB_MAGIC, 2, 20 + len(payload))
    mesh += struct.pack('<II', len(payload), oracle.GLB_JSON_CHUNK) + payload
    (root / 'meshes/rock.glb').write_bytes(mesh)
    entry = {'id': 'mudcrab.material', 'version': 1, 'owns': {'gltf_extensions': ['MUDCRAB_material']}}
    assert oracle.extension_hash(root, entry) == '714a8bfd6413db8bf63f22174dfb3f5c173a8126f93625d1c63a00b7df144567'
    node = {'extensionsUsed': ['EXAMPLE_payload'],
            'nodes': [{'extensions': {'EXAMPLE_payload': {'value': 1}}}]}
    primitive = {'extensionsUsed': ['EXAMPLE_payload'],
                 'meshes': [{'primitives': [{'extensions': {'EXAMPLE_payload': {'value': 1}}}]}]}
    assert oracle.gltf_extension_value(node, 'EXAMPLE_payload') is not None
    assert oracle.gltf_extension_value(node, 'EXAMPLE_payload') != oracle.gltf_extension_value(primitive, 'EXAMPLE_payload')
    for alias in ['meshes/./rock.glb', 'meshes/../meshes/rock.glb', 'meshes//rock.glb',
                  'meshes\\rock.glb', '../outside.bin', '/absolute.bin', 'Q:/absolute.bin', '']:
        try:
            oracle.owned_files(root, [alias])
        except ValueError:
            pass
        else:
            raise AssertionError(f'accepted noncanonical owned path: {alias!r}')
"#;
    let result = Command::new(python)
        .arg("-I")
        .arg("-c")
        .arg(code)
        .arg(script)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
