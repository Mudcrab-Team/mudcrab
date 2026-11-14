use super::*;
use shared::records::{ArchivedRecordData, ArchivedSubRecord};

const NORD: u32 = 0x200;
const DEFAULT_RACE: u32 = 25;

fn blob(subrecords: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let data = ArchivedRecordData {
        subrecords: subrecords
            .iter()
            .map(|(tag, data)| ArchivedSubRecord {
                tag: **tag,
                data: data.clone(),
            })
            .collect(),
    };
    rkyv::to_bytes::<rkyv::rancor::Error>(&data)
        .unwrap()
        .to_vec()
}

fn ids(ids: &[u32]) -> Vec<u8> {
    ids.iter().flat_map(|id| id.to_le_bytes()).collect()
}

fn text(value: &str) -> Vec<u8> {
    let mut bytes = value.as_bytes().to_vec();
    bytes.push(0);
    bytes
}

/// A world database with one Nord actor in a blacksmith outfit (torso and shoes).
fn write_database(path: &Path) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(&format!(
            "CREATE TABLE schema_info (version INTEGER NOT NULL);
             INSERT INTO schema_info VALUES ({});
             CREATE TABLE records (form_id INTEGER PRIMARY KEY, record_type TEXT NOT NULL,
               cell_id INTEGER, worldspace_id INTEGER, load_order INTEGER NOT NULL, data BLOB NOT NULL);
             CREATE TABLE actor_models (id INTEGER PRIMARY KEY, race_id INTEGER NOT NULL,
               sex TEXT NOT NULL, skin_armo_id INTEGER, head_model TEXT, head_source TEXT,
               resolution TEXT NOT NULL);
             INSERT INTO actor_models(id, race_id, sex, resolution) VALUES (256, {NORD}, 'male', 'body_head');
             INSERT INTO actor_models(id, race_id, sex, resolution) VALUES (257, {NORD}, 'male', 'unresolved');",
            shared::WORLD_DATABASE_SCHEMA_VERSION
        ))
        .unwrap();
    let records: Vec<(u32, &str, Vec<u8>)> = vec![
        (
            0x100,
            "NPC_",
            blob(&[(b"ACBS", vec![0; 24]), (b"DOFT", ids(&[0x300]))]),
        ),
        (
            0x101,
            "NPC_",
            blob(&[(b"ACBS", vec![0; 24]), (b"DOFT", ids(&[0x300]))]),
        ),
        (
            0x300,
            "OTFT",
            blob(&[(b"INAM", ids(&[0x400, 0x401, 0x402]))]),
        ),
        (0x400, "ARMO", blob(&[(b"MODL", ids(&[0x500]))])),
        (0x401, "ARMO", blob(&[(b"MODL", ids(&[0x501]))])),
        (0x402, "LVLI", blob(&[])),
        (
            0x500,
            "ARMA",
            blob(&[
                (b"BOD2", [4u32.to_le_bytes(), 2u32.to_le_bytes()].concat()),
                (b"RNAM", ids(&[DEFAULT_RACE])),
                (b"MOD2", text("Clothes\\BlackSmith\\M\\Torso_1.nif")),
            ]),
        ),
        (
            0x501,
            "ARMA",
            blob(&[
                (
                    b"BOD2",
                    [0x80u32.to_le_bytes(), 2u32.to_le_bytes()].concat(),
                ),
                (b"RNAM", ids(&[DEFAULT_RACE])),
                (b"MOD2", text("Clothes\\BlackSmith\\M\\Shoes_1.nif")),
            ]),
        ),
    ];
    for (id, kind, data) in records {
        connection
            .execute(
                "INSERT INTO records(form_id, record_type, load_order, data) VALUES (?1, ?2, 0, ?3)",
                rusqlite::params![id, kind, data],
            )
            .unwrap();
    }
}

struct Fixture {
    directory: tempfile::TempDir,
    registry: PathBuf,
    registered: PathBuf,
    clone: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let registered = directory.path().join("registered");
        let clone = directory.path().join("clone");
        fs::create_dir_all(&registered).unwrap();
        write_database(&registered.join("skyrim_world.db"));
        fs::write(
            registered.join("conversion-manifest.json"),
            r#"{"schema_version":28,"complete":true}"#,
        )
        .unwrap();
        fs::write(registered.join("integration-report.json"), "{}").unwrap();
        let scene = registered.join("meshes/clothes/blacksmith/m/torso_1.glb");
        fs::create_dir_all(scene.parent().unwrap()).unwrap();
        fs::write(scene, b"glb").unwrap();
        // The test stands in for `cp -c -R`: a byte copy, never a link.
        copy_directory(&registered, &clone);
        let registry = directory.path().join("packs.json");
        let manifests: BTreeMap<_, _> = ["conversion-manifest.json", "integration-report.json"]
            .iter()
            .map(|name| {
                (
                    (*name).to_owned(),
                    hash_file(&registered.join(name)).unwrap(),
                )
            })
            .collect();
        fs::write(
            &registry,
            serde_json::json!({"base": {"path": registered, "manifests": manifests}}).to_string(),
        )
        .unwrap();
        Self {
            directory,
            registry,
            registered,
            clone,
        }
    }

    fn options(&self) -> DeriveOutfitsOptions {
        DeriveOutfitsOptions {
            target: self.clone.clone(),
            registry: self.registry.clone(),
            clone_of: "base".into(),
            replace: false,
            converter_head: Some("deadbeef".into()),
            command: vec!["converter".into(), "derive-outfits".into()],
        }
    }
}

fn copy_directory(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_directory(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn refusal(fixture: &Fixture, options: &DeriveOutfitsOptions) -> String {
    let error = run(options).unwrap_err().to_string();
    assert!(error.starts_with("refused:"), "{error}");
    assert!(
        !fixture.clone.join(CATALOG_FILE).exists(),
        "a refusal must write nothing"
    );
    error
}

#[test]
fn derives_the_catalog_and_a_receipt_into_the_clone_only() {
    let fixture = Fixture::new();
    let registered_before = fs::read(fixture.registered.join("skyrim_world.db")).unwrap();
    let report = run(&fixture.options()).unwrap();
    assert_eq!(
        report.actors_with_outfit, 1,
        "the unresolved actor wears nothing"
    );
    assert_eq!(report.parts, 1, "the shoes model is not in the pack");
    assert_eq!(report.skipped["converted_model_missing"], 1);
    assert_eq!(report.skipped["leveled_list"], 1);

    let catalog: ActorOutfitCatalog =
        serde_json::from_slice(&fs::read(fixture.clone.join(CATALOG_FILE)).unwrap()).unwrap();
    assert_eq!(catalog.validate(), Ok(()));
    let outfit = catalog.find(0x100).unwrap();
    assert_eq!(
        outfit.parts[0].scene,
        "meshes/clothes/blacksmith/m/torso_1.glb"
    );
    assert!(catalog.find(0x101).is_none());

    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.clone.join(RECEIPT_FILE)).unwrap()).unwrap();
    assert_eq!(receipt["format"], RECEIPT_FORMAT);
    assert_eq!(receipt["game_data_read"], false);
    assert_eq!(receipt["converter_head"], "deadbeef");
    assert_eq!(receipt["catalog"]["parts"], 1);
    assert_eq!(receipt["clone_of"]["name"], "base");
    assert!(report.bytes_added > 0);

    // The registered pack is untouched, and the clone gained exactly two files.
    assert_eq!(
        fs::read(fixture.registered.join("skyrim_world.db")).unwrap(),
        registered_before
    );
    assert!(!fixture.registered.join(CATALOG_FILE).exists());
    assert!(!fixture.clone.join("skyrim_world.db-wal").exists());
    assert!(!fixture.clone.join("skyrim_world.db-journal").exists());
    let names = top_level_names(&fixture.clone).unwrap();
    assert!(names.contains(CATALOG_FILE) && names.contains(RECEIPT_FILE));
    assert!(!names.contains("actor-outfits.json.tmp"));
}

#[test]
fn refuses_the_registered_pack_a_path_inside_it_and_a_path_around_it() {
    let fixture = Fixture::new();
    let mut options = fixture.options();
    options.target = fixture.registered.clone();
    assert!(refusal(&fixture, &options).contains("is the registered pack"));
    assert!(!fixture.registered.join(CATALOG_FILE).exists());

    fs::create_dir_all(fixture.registered.join("inner")).unwrap();
    options.target = fixture.registered.join("inner");
    assert!(refusal(&fixture, &options).contains("is inside the registered pack"));

    options.target = fixture.directory.path().to_path_buf();
    assert!(refusal(&fixture, &options).contains("contains the registered pack"));
}

#[test]
fn refuses_an_unknown_pack_a_missing_target_and_a_symlink() {
    let fixture = Fixture::new();
    let mut options = fixture.options();
    options.clone_of = "other".into();
    assert!(refusal(&fixture, &options).contains("is not a registered pack"));

    let mut options = fixture.options();
    options.target = fixture.directory.path().join("absent");
    assert!(refusal(&fixture, &options).contains("does not exist"));

    #[cfg(unix)]
    {
        let link = fixture.directory.path().join("link");
        std::os::unix::fs::symlink(&fixture.clone, &link).unwrap();
        let mut options = fixture.options();
        options.target = link;
        assert!(refusal(&fixture, &options).contains("not a file or a symlink"));
    }
}

#[test]
fn refuses_a_target_that_is_not_an_exact_clone() {
    let fixture = Fixture::new();
    fs::write(fixture.clone.join("integration-report.json"), "{\"x\":1}").unwrap();
    assert!(refusal(&fixture, &fixture.options()).contains("not an exact clone"));

    fs::remove_file(fixture.clone.join("integration-report.json")).unwrap();
    assert!(refusal(&fixture, &fixture.options()).contains("has no integration-report.json"));
}

#[cfg(unix)]
#[test]
fn refuses_a_hard_link_to_the_registered_file() {
    let fixture = Fixture::new();
    let linked = fixture.clone.join("conversion-manifest.json");
    fs::remove_file(&linked).unwrap();
    fs::hard_link(fixture.registered.join("conversion-manifest.json"), &linked).unwrap();
    assert!(refusal(&fixture, &fixture.options()).contains("use `cp -c -R`"));
}

#[test]
fn refuses_an_incomplete_manifest_after_the_clone_is_proven() {
    let fixture = Fixture::new();
    let manifest = fixture.registered.join("conversion-manifest.json");
    fs::write(&manifest, r#"{"schema_version":28,"complete":false}"#).unwrap();
    copy_directory(&fixture.registered, &fixture.clone);
    // Re-register the changed manifest so the clone proof passes and only completeness fails.
    let registry = fs::read_to_string(&fixture.registry).unwrap();
    let mut registry: serde_json::Value = serde_json::from_str(&registry).unwrap();
    registry["base"]["manifests"]["conversion-manifest.json"] =
        hash_file(&manifest).unwrap().into();
    fs::write(&fixture.registry, registry.to_string()).unwrap();
    assert!(refusal(&fixture, &fixture.options()).contains("is not complete"));
}

#[test]
fn an_existing_catalog_needs_replace() {
    let fixture = Fixture::new();
    run(&fixture.options()).unwrap();
    let error = run(&fixture.options()).unwrap_err().to_string();
    assert!(error.contains("pass --replace"), "{error}");
    let mut options = fixture.options();
    options.replace = true;
    run(&options).unwrap();
}

#[test]
fn refuses_a_database_the_derive_cannot_read() {
    let fixture = Fixture::new();
    Connection::open(fixture.clone.join("skyrim_world.db"))
        .unwrap()
        .execute_batch("DROP TABLE actor_models")
        .unwrap();
    // The clone proof covers the manifests, so the edited database reaches the table check.
    let error = run(&fixture.options()).unwrap_err().to_string();
    assert!(error.contains("has no `actor_models` table"), "{error}");
    assert!(!fixture.clone.join(CATALOG_FILE).exists());
}

#[test]
fn refuses_a_database_without_outfit_records() {
    let fixture = Fixture::new();
    Connection::open(fixture.clone.join("skyrim_world.db"))
        .unwrap()
        .execute_batch("DELETE FROM records WHERE record_type='OTFT'")
        .unwrap();
    let error = run(&fixture.options()).unwrap_err().to_string();
    assert!(error.contains("no OTFT records"), "{error}");
}

#[test]
fn a_foreign_world_schema_is_refused() {
    let fixture = Fixture::new();
    Connection::open(fixture.clone.join("skyrim_world.db"))
        .unwrap()
        .execute_batch("UPDATE schema_info SET version = 1")
        .unwrap();
    let error = run(&fixture.options()).unwrap_err().to_string();
    assert!(error.contains("outside the runtime range"), "{error}");
}
