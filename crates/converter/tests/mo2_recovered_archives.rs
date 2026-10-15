//! MO2 archive ownership recovery through synthetic DDS conversion, cache reuse and publication.

use converter::{
    AssetPipeline, PipelineConfig, PipelineReport, TextureEncoder,
    cache::{ConversionManifest, hash_bytes, hash_file},
    esm::exporter::validate_database,
    repair::repair_failed,
};
use dummy_content::{Entry, bsa, dds, rng::Rng};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Keep every profile and converted pack isolated, with distinguishable valid texture sources.
struct Fixture {
    directory: tempfile::TempDir,
    data: PathBuf,
    instance: PathBuf,
    output: PathBuf,
    broken_plugin: PathBuf,
    good_dds: Vec<u8>,
}

impl Fixture {
    /// Create direct virtual plugin owners whose physical paths live in enabled mods or overwrite.
    fn new(overwrite_owner: bool, missing_master: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let instance = directory.path().join("MO2");
        let output = directory.path().join("pack");
        for path in [
            data.clone(),
            instance.join("mods/Owners"),
            instance.join("mods/Archives"),
            instance.join("profiles/Default"),
            instance.join("overwrite"),
        ] {
            fs::create_dir_all(path).unwrap();
        }
        fs::write(data.join("Skyrim.esm"), record(b"TES4", 0, Vec::new())).unwrap();
        plugin(&instance.join("mods/Owners/Good.esp"), "Skyrim.esm", true);
        let broken_plugin = instance.join(if overwrite_owner {
            "overwrite/Good - Broken.esp"
        } else {
            "mods/Owners/Good - Broken.esp"
        });
        plugin(
            &broken_plugin,
            if missing_master {
                "Missing.esm"
            } else {
                "Skyrim.esm"
            },
            false,
        );
        fs::write(
            instance.join("ModOrganizer.ini"),
            "[General]\ngameName=Skyrim Special Edition\n",
        )
        .unwrap();
        fs::write(
            instance.join("profiles/Default/modlist.txt"),
            "+Archives\n+Owners\n",
        )
        .unwrap();
        fs::write(
            instance.join("profiles/Default/plugins.txt"),
            "*Good.esp\n*Good - Broken.esp\n",
        )
        .unwrap();
        fs::write(
            instance.join("profiles/Default/loadorder.txt"),
            "Skyrim.esm\nGood.esp\nGood - Broken.esp\n",
        )
        .unwrap();
        let good_dds = texture(4, 11);
        let bad_dds = texture(8, 29);
        let bad_suffix_dds = texture(16, 47);
        let standalone_dds = texture(12, 71);
        let archives = instance.join("mods/Archives");
        archive(
            &archives.join("Good.bsa"),
            &[
                Entry::new("textures/shared.dds", &good_dds),
                Entry::new("textures/neighbor.dds", &good_dds),
            ],
        );
        archive(
            &archives.join("Good - Broken.bsa"),
            &[
                Entry::new("textures/shared.dds", &bad_dds),
                Entry::new("textures/rejected_only.dds", &bad_dds),
            ],
        );
        archive(
            &archives.join("Good - Broken - Textures.bsa"),
            &[Entry::new("textures/shared.dds", &bad_suffix_dds)],
        );
        archive(
            &archives.join("Standalone.bsa"),
            &[Entry::new("textures/standalone.dds", &standalone_dds)],
        );
        Self {
            directory,
            data,
            instance,
            output,
            broken_plugin,
            good_dds,
        }
    }

    /// Configure CPU-only synthetic conversion; no retail assets or graphics device are used.
    fn config(&self) -> PipelineConfig {
        let mut config = PipelineConfig::new(&self.data, &self.output);
        config.mo2 = Some(mo2::Selection {
            instance_path: self.instance.clone(),
            profile: "Default".into(),
        });
        config.no_lod = true;
        config.cpu_jobs = 2;
        config.texture_encoder = TextureEncoder::Cpu;
        config
    }

    /// Read the published conversion manifest, including archive and source-hash provenance.
    fn manifest(&self) -> serde_json::Value {
        serde_json::from_slice(&fs::read(self.output.join("conversion-manifest.json")).unwrap())
            .unwrap()
    }

    /// Verify DDS dimensions survive actual CPU encoding and KTX2 publication.
    fn assert_texture(&self, relative: &str, width: u32) {
        assert_texture_path(&self.output.join(relative), width);
    }

    /// Verify recovery preserves the neighboring plugin, database validity and good archive bytes.
    fn assert_recovered_publication(&self, report: &PipelineReport) {
        assert!(report.complete && report.skipped == 0, "{report:?}");
        let connection = rusqlite::Connection::open(self.output.join("skyrim_world.db")).unwrap();
        validate_database(&connection).unwrap();
        let names: Vec<String> = connection
            .prepare("SELECT name FROM plugins ORDER BY priority")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(names, ["Skyrim.esm", "Good.esp"]);
        let value: f64 = connection
            .query_row(
                "SELECT value FROM movement_game_settings WHERE editor_id='fJumpHeightMin'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(value, 222.0);
        self.assert_texture("textures/shared.ktx2", 4);
        self.assert_texture("textures/neighbor.ktx2", 4);
        let manifest = self.manifest();
        assert_eq!(manifest["complete"], true);
        assert!(manifest["archives"]["good.bsa"].is_object());
        assert!(manifest["archives"].get("good - broken.bsa").is_none());
        assert!(
            manifest["archives"]
                .get("good - broken - textures.bsa")
                .is_none()
        );
        assert_dds_source(&manifest, "textures/shared.dds", &self.good_dds);
        assert!(!self.output.join("textures/rejected_only.ktx2").exists());
        assert!(report.notices.iter().any(|notice| {
            let notice = notice.to_ascii_lowercase();
            notice.contains("good - broken.esp") && notice.contains("missing.esm")
        }));
    }
}

/// Encode a native plugin record header around a synthetic payload.
fn record(tag: &[u8; 4], id: u32, payload: Vec<u8>) -> Vec<u8> {
    [
        tag.as_slice(),
        &(payload.len() as u32).to_le_bytes(),
        &[0; 4],
        &id.to_le_bytes(),
        &[0; 8],
        &payload,
    ]
    .concat()
}

/// Encode a native subrecord without importing any Skyrim-owned bytes.
fn sub(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    [
        tag.as_slice(),
        &(payload.len() as u16).to_le_bytes(),
        payload,
    ]
    .concat()
}

/// Write a usable master dependency and a valid neighboring jump setting when requested.
fn plugin(path: &Path, master: &str, neighboring_setting: bool) {
    let header = [
        sub(b"MAST", format!("{master}\0").as_bytes()),
        sub(b"DATA", &[0; 8]),
    ]
    .concat();
    let mut bytes = record(b"TES4", 0, header);
    if neighboring_setting {
        bytes.extend(record(
            b"GMST",
            0x01000800,
            [
                sub(b"EDID", b"fJumpHeightMin\0"),
                sub(b"DATA", &222.0f32.to_le_bytes()),
            ]
            .concat(),
        ));
    }
    fs::write(path, bytes).unwrap();
}

/// Generate asymmetric, valid DDS payloads whose widths and source hashes identify the winning archive.
fn texture(width: u32, seed: u64) -> Vec<u8> {
    dds::generate(
        &dds::Spec::new(dds::Format::X8R8G8B8, width, 4),
        &mut Rng::new(seed),
    )
    .unwrap()
}

/// Inspect actual published or previewed KTX2 dimensions independently of its source metadata.
fn assert_texture_path(path: &Path, width: u32) {
    let bytes = fs::read(path).unwrap();
    let header = ktx2::Reader::new(bytes.as_slice()).unwrap().header();
    assert_eq!(
        header.pixel_width,
        width,
        "wrong archive supplied {}",
        path.display()
    );
    assert_eq!(header.pixel_height, 4);
}

/// Verify the raw DDS digest independently of an appended encoding recipe.
fn assert_dds_source(manifest: &serde_json::Value, key: &str, source: &[u8]) {
    let raw_hash = hash_bytes(source);
    let recorded = manifest["entries"][key]["source_hash"].as_str().unwrap();
    assert!(
        recorded == raw_hash || recorded.starts_with(&format!("{raw_hash}:texture-encoding:")),
        "wrong DDS source for {key}: {recorded}"
    );
}

/// Read recorded plugin provenance while checking database validity and the healthy neighboring record.
fn recorded_plugin_provenance(output: &Path) -> Vec<(String, i64, String)> {
    let connection = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
    validate_database(&connection).unwrap();
    let version: u32 = connection
        .query_row("SELECT version FROM schema_info", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, shared::WORLD_DATABASE_SCHEMA_VERSION);
    let good: f64 = connection
        .query_row(
            "SELECT value FROM movement_game_settings WHERE editor_id='fJumpHeightMin'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(good, 222.0);
    let mut statement = connection
        .prepare("SELECT name,priority,lower(hex(checksum)) FROM plugins ORDER BY priority")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Select the same fixture through a player list after copying virtual-root owners and archives into Data.
fn player_config(fixture: &Fixture) -> PipelineConfig {
    for name in ["Good.esp", "Good - Broken.esp"] {
        fs::copy(
            fixture.instance.join("mods/Owners").join(name),
            fixture.data.join(name),
        )
        .unwrap();
    }
    for name in [
        "Good.bsa",
        "Good - Broken.bsa",
        "Good - Broken - Textures.bsa",
        "Standalone.bsa",
    ] {
        fs::copy(
            fixture.instance.join("mods/Archives").join(name),
            fixture.data.join(name),
        )
        .unwrap();
    }
    let list = fixture.directory.path().join("plugins.txt");
    fs::write(&list, "*Good.esp\n*Good - Broken.esp\n").unwrap();
    let mut config = fixture.config();
    config.mo2 = None;
    config.plugins_file = Some(list);
    config
}

/// Package real synthetic DDS inputs in an uncompressed Skyrim SE BSA.
fn archive(path: &Path, entries: &[Entry<'_>]) {
    fs::write(path, bsa::v105(entries, bsa::Compression::None).unwrap()).unwrap();
}

/// Run the real pipeline and drain its bounded progress channel through publication.
async fn publish(config: PipelineConfig) -> PipelineReport {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<converter::ProgressEvent>(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let result = AssetPipeline::run_async(config, tx).await;
    drain.await.unwrap();
    let report = result.unwrap();
    assert!(report.complete && report.skipped == 0, "{report:?}");
    report
}

/// A recovered longest owner in an enabled mod or overwrite must suppress its exact and suffixed archives.
#[tokio::test]
async fn rejected_longer_mo2_owner_preserves_neighboring_dds_and_database() {
    for overwrite_owner in [false, true] {
        let fixture = Fixture::new(overwrite_owner, true);
        let report = publish(fixture.config()).await;
        fixture.assert_recovered_publication(&report);
        assert!(!fixture.output.join("textures/standalone.ktx2").exists());
        assert!(
            fixture.manifest()["archives"]
                .get("standalone.bsa")
                .is_none()
        );
    }
}

/// Cached prior admission and resumed rejected outputs cannot reactivate a newly recovered-away owner.
#[tokio::test]
async fn resume_drops_rejected_mo2_owner_from_warm_archive_cache() {
    let fixture = Fixture::new(true, false);
    publish(fixture.config()).await;
    fixture.assert_texture("textures/shared.ktx2", 16);
    assert!(fixture.manifest()["archives"]["good - broken.bsa"].is_object());
    assert!(fixture.manifest()["archives"]["good - broken - textures.bsa"].is_object());
    let staging = fixture.directory.path().join("pack.staging-233-1");
    fs::create_dir_all(staging.join("vfs/textures")).unwrap();
    fs::create_dir_all(staging.join("textures")).unwrap();
    for name in ["shared.ktx2", "neighbor.ktx2", "rejected_only.ktx2"] {
        fs::copy(
            fixture.output.join("textures").join(name),
            staging.join("textures").join(name),
        )
        .unwrap();
    }
    fs::write(
        staging.join("vfs/textures/rejected_only.dds"),
        texture(8, 29),
    )
    .unwrap();
    plugin(&fixture.broken_plugin, "Missing.esm", false);
    let mut config = fixture.config();
    config.resume_staging = Some(staging.clone());
    let report = publish(config).await;
    // Good.bsa restores two DDS entries; only the unchanged neighbor KTX2 can reuse an asset hit.
    // The shared texture changes from 16 to 4 pixels and must be converted again.
    assert_eq!(
        report.cache_hits, 3,
        "expected two restored archive entries and one unchanged texture reuse"
    );
    fixture.assert_recovered_publication(&report);
    assert!(!staging.exists());
}

/// Standalone admission differs deliberately between MO2 and player selection, while ownership recovery agrees.
#[tokio::test]
async fn non_mo2_selection_retains_unowned_archive_with_the_same_recovered_owners() {
    let fixture = Fixture::new(false, true);
    let report = publish(player_config(&fixture)).await;
    fixture.assert_recovered_publication(&report);
    fixture.assert_texture("textures/standalone.ktx2", 12);
    assert!(fixture.manifest()["archives"]["standalone.bsa"].is_object());
}

/// Repair preview and application obey the same recovered owner policy across MO2 layers and player Data.
#[tokio::test]
async fn repair_uses_recovered_archive_owners_for_mo2_and_player_selection() {
    for (overwrite_owner, player_selection) in [(false, false), (true, false), (false, true)] {
        let fixture = Fixture::new(overwrite_owner, true);
        let config = if player_selection {
            player_config(&fixture)
        } else {
            fixture.config()
        };
        let conversion = publish(config.clone()).await;
        fixture.assert_recovered_publication(&conversion);
        let manifest_path = fixture.output.join("conversion-manifest.json");
        let mut manifest: ConversionManifest =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest.failures.insert(
            "textures/shared.dds".into(),
            "synthetic retry of an archived texture after owner recovery".into(),
        );
        manifest.complete = false;
        manifest.save(&manifest_path).unwrap();
        let before_manifest = fs::read(&manifest_path).unwrap();
        let before_texture = fs::read(fixture.output.join("textures/shared.ktx2")).unwrap();
        let preview = repair_failed(&config, false).unwrap();
        assert!(!preview.published && preview.failures.is_empty());
        assert_eq!(preview.converted, 1);
        let preview_assets = preview.directory.join("assets");
        assert_texture_path(&preview_assets.join("textures/shared.ktx2"), 4);
        let preview_manifest: serde_json::Value = serde_json::from_slice(
            &fs::read(preview_assets.join("conversion-manifest.json")).unwrap(),
        )
        .unwrap();
        assert_dds_source(&preview_manifest, "textures/shared.dds", &fixture.good_dds);
        assert_eq!(fs::read(&manifest_path).unwrap(), before_manifest);
        assert_eq!(
            fs::read(fixture.output.join("textures/shared.ktx2")).unwrap(),
            before_texture
        );
        let applied = repair_failed(&config, true).unwrap();
        assert!(applied.published && applied.failures.is_empty());
        assert_eq!(applied.converted, 1);
        fixture.assert_recovered_publication(&conversion);
        if player_selection {
            fixture.assert_texture("textures/standalone.ktx2", 12);
        } else {
            assert!(!fixture.output.join("textures/standalone.ktx2").exists());
        }
    }
}

/// Previously admitted cached companions must not supply replacement bytes after owner recovery.
#[tokio::test]
async fn warm_repair_excludes_cached_recovered_owner_and_preserves_pack_provenance() {
    for (overwrite_owner, player_selection) in [(false, false), (true, false), (false, true)] {
        let fixture = Fixture::new(overwrite_owner, false);
        let config = if player_selection {
            player_config(&fixture)
        } else {
            fixture.config()
        };
        publish(config.clone()).await;
        fixture.assert_texture("textures/shared.ktx2", 16);
        fixture.assert_texture("textures/neighbor.ktx2", 4);
        fixture.assert_texture("textures/rejected_only.ktx2", 8);
        let recorded_plugins = recorded_plugin_provenance(&fixture.output);
        assert_eq!(
            recorded_plugins
                .iter()
                .map(|(name, _, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["Skyrim.esm", "Good.esp", "Good - Broken.esp"]
        );
        let manifest_path = fixture.output.join("conversion-manifest.json");
        let mut manifest: ConversionManifest =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        let cache = config.ingestion_cache_dir().join(".ingestion-cache");
        for (archive_name, expected) in [
            ("good - broken.bsa", texture(8, 29)),
            ("good - broken - textures.bsa", texture(16, 47)),
        ] {
            let entry = &manifest.archives[archive_name];
            let shared = entry
                .files
                .iter()
                .find(|file| file.path == "textures/shared.dds")
                .unwrap();
            assert_eq!(shared.hash, hash_bytes(&expected));
            for file in &entry.files {
                assert_eq!(file.hash.len(), 64);
                let blob = cache.join("sha256").join(&file.hash[..2]).join(&file.hash);
                assert_eq!(hash_file(&blob).unwrap(), file.hash);
            }
        }
        assert_dds_source(&fixture.manifest(), "textures/shared.dds", &texture(16, 47));

        // Keep real admitted archive/cache entries and output bytes; only the winning header changes.
        let rejected_owner = if player_selection {
            config.data_dir.join("Good - Broken.esp")
        } else {
            fixture.broken_plugin.clone()
        };
        plugin(&rejected_owner, "Missing.esm", false);
        manifest.failures.insert(
            "textures/shared.dds".into(),
            "synthetic retry after a previously admitted cached owner became unusable".into(),
        );
        manifest.complete = false;
        manifest.save(&manifest_path).unwrap();
        let original_manifest = fs::read(&manifest_path).unwrap();
        let original_shared = fs::read(fixture.output.join("textures/shared.ktx2")).unwrap();
        let original_database = fs::read(fixture.output.join("skyrim_world.db")).unwrap();
        let original_neighbor = hash_file(&fixture.output.join("textures/neighbor.ktx2")).unwrap();
        let unrelated_rejected =
            hash_file(&fixture.output.join("textures/rejected_only.ktx2")).unwrap();

        let preview = repair_failed(&config, false).unwrap();
        assert!(!preview.published && preview.failures.is_empty());
        assert_eq!(preview.converted, 1);
        let preview_assets = preview.directory.join("assets");
        assert_texture_path(&preview_assets.join("textures/shared.ktx2"), 4);
        let preview_manifest: serde_json::Value = serde_json::from_slice(
            &fs::read(preview_assets.join("conversion-manifest.json")).unwrap(),
        )
        .unwrap();
        assert_dds_source(&preview_manifest, "textures/shared.dds", &fixture.good_dds);
        assert_eq!(
            recorded_plugin_provenance(&preview_assets),
            recorded_plugins
        );
        assert_eq!(fs::read(&manifest_path).unwrap(), original_manifest);
        assert_eq!(
            fs::read(fixture.output.join("textures/shared.ktx2")).unwrap(),
            original_shared
        );
        assert_eq!(
            fs::read(fixture.output.join("skyrim_world.db")).unwrap(),
            original_database
        );

        let applied = repair_failed(&config, true).unwrap();
        assert!(applied.published && applied.failures.is_empty());
        assert_eq!(applied.converted, 1);
        fixture.assert_texture("textures/shared.ktx2", 4);
        assert_dds_source(
            &fixture.manifest(),
            "textures/shared.dds",
            &fixture.good_dds,
        );
        assert_eq!(
            recorded_plugin_provenance(&fixture.output),
            recorded_plugins
        );
        assert_eq!(
            hash_file(&fixture.output.join("textures/neighbor.ktx2")).unwrap(),
            original_neighbor
        );
        // Asset repair retains recorded database provenance and unrelated previously published assets.
        assert_eq!(
            hash_file(&fixture.output.join("textures/rejected_only.ktx2")).unwrap(),
            unrelated_rejected
        );
        assert!(
            fixture.manifest()["failures"]
                .as_object()
                .unwrap()
                .is_empty()
        );
        assert_eq!(fixture.manifest()["complete"], true);
    }
}

/// An active longest owner after an intervening plugin must keep its later archive override priority.
#[tokio::test]
async fn longest_active_mo2_owner_retains_its_later_archive_priority() {
    let fixture = Fixture::new(false, true);
    for name in ["Good - Broken.bsa", "Good - Broken - Textures.bsa"] {
        fs::remove_file(fixture.instance.join("mods/Archives").join(name)).unwrap();
    }
    plugin(
        &fixture.instance.join("mods/Owners/Middle.esp"),
        "Skyrim.esm",
        false,
    );
    plugin(
        &fixture.instance.join("overwrite/Good - Expansion.esp"),
        "Skyrim.esm",
        false,
    );
    fs::write(
        fixture.instance.join("profiles/Default/plugins.txt"),
        "*Good.esp\n*Middle.esp\n*Good - Expansion.esp\n",
    )
    .unwrap();
    fs::write(
        fixture.instance.join("profiles/Default/loadorder.txt"),
        "Skyrim.esm\nGood.esp\nMiddle.esp\nGood - Expansion.esp\n",
    )
    .unwrap();
    let middle_dds = texture(8, 83);
    let expansion_dds = texture(16, 97);
    archive(
        &fixture.instance.join("mods/Archives/Middle.bsa"),
        &[Entry::new("textures/shared.dds", &middle_dds)],
    );
    archive(
        &fixture.instance.join("mods/Archives/Good - Expansion.bsa"),
        &[Entry::new("textures/shared.dds", &expansion_dds)],
    );
    publish(fixture.config()).await;
    let connection = rusqlite::Connection::open(fixture.output.join("skyrim_world.db")).unwrap();
    validate_database(&connection).unwrap();
    let names: Vec<String> = connection
        .prepare("SELECT name FROM plugins ORDER BY priority")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        names,
        [
            "Skyrim.esm",
            "Good.esp",
            "Middle.esp",
            "Good - Expansion.esp"
        ]
    );
    fixture.assert_texture("textures/shared.ktx2", 16);
    fixture.assert_texture("textures/neighbor.ktx2", 4);
    let manifest = fixture.manifest();
    assert!(manifest["archives"]["good - expansion.bsa"].is_object());
    assert!(manifest["archives"]["middle.bsa"].is_object());
    assert_dds_source(&manifest, "textures/shared.dds", &expansion_dds);
}
