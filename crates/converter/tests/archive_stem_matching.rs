//! Shared archive ownership, activity and priority through actual synthetic publication.

use converter::{
    AssetPipeline, PipelineConfig, PipelineReport, TextureEncoder,
    cache::{ConversionManifest, hash_bytes, hash_file},
    esm::exporter::validate_database,
    repair::repair_failed,
};
use dummy_content::{Entry, bsa, dds, layout, rng::Rng};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Exercise physical player inputs and both winning MO2 owner locations.
#[derive(Clone, Copy, Debug)]
enum Selection {
    Automatic,
    Player,
    Mo2Mod,
    Mo2Overwrite,
}

impl Selection {
    /// Identify profiles whose sources are resolved through a virtual root.
    fn is_mo2(self) -> bool {
        matches!(self, Self::Mo2Mod | Self::Mo2Overwrite)
    }
}

/// Keep independent, proprietary-free source and output trees under the configured TEMP.
struct Fixture {
    directory: tempfile::TempDir,
    data: PathBuf,
    instance: PathBuf,
    output: PathBuf,
    selection: Selection,
}

impl Fixture {
    /// Create a readable empty CCC file to isolate archive behavior from fallback warnings.
    fn new(selection: Selection) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let instance = directory.path().join("MO2");
        for path in [
            data.clone(),
            instance.join("mods/Owners"),
            instance.join("mods/Archives"),
            instance.join("profiles/Default"),
            instance.join("overwrite"),
        ] {
            fs::create_dir_all(path).unwrap();
        }
        fs::write(directory.path().join("Skyrim.ccc"), b"").unwrap();
        fs::write(data.join("Skyrim.esm"), plugin_bytes(None, None)).unwrap();
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
        let output = directory.path().join("pack");
        let fixture = Self {
            directory,
            data,
            instance,
            output,
            selection,
        };
        fixture.plugin("Good.esp", "Skyrim.esm", Some(222.0));
        fixture.select(&["Good.esp"]);
        fixture
    }

    /// Return the winning installed owner path independently of dependency recovery.
    fn owner_path(&self, name: &str) -> PathBuf {
        if !self.selection.is_mo2() {
            self.data.join(name)
        } else if matches!(self.selection, Selection::Mo2Overwrite) && name != "Good.esp" {
            self.instance.join("overwrite").join(name)
        } else {
            self.instance.join("mods/Owners").join(name)
        }
    }

    /// Write a usable header and optional healthy neighboring GMST record.
    fn plugin(&self, name: &str, master: &str, setting: Option<f32>) {
        fs::write(self.owner_path(name), plugin_bytes(Some(master), setting)).unwrap();
    }

    /// Keep explicit player and profile lists stable while a dependency changes.
    fn select(&self, names: &[&str]) {
        let selected = names
            .iter()
            .map(|name| format!("*{name}\n"))
            .collect::<String>();
        fs::write(self.directory.path().join("plugins.txt"), &selected).unwrap();
        fs::write(self.instance.join("profiles/Default/plugins.txt"), selected).unwrap();
        fs::write(
            self.instance.join("profiles/Default/loadorder.txt"),
            format!("Skyrim.esm\n{}\n", names.join("\n")),
        )
        .unwrap();
    }

    /// Publish recognizable DDS resources from a physical or virtual archive source.
    fn archive(&self, name: &str, entries: &[Entry<'_>]) {
        let root = if self.selection.is_mo2() {
            self.instance.join("mods/Archives")
        } else {
            self.data.clone()
        };
        fs::write(
            root.join(name),
            bsa::v105(entries, bsa::Compression::None).unwrap(),
        )
        .unwrap();
    }

    /// Configure explicit CPU-only synthetic conversion with bounded worker counts.
    fn config(&self) -> PipelineConfig {
        let mut config = cpu_config(&self.data, &self.output);
        if matches!(self.selection, Selection::Player) {
            config.plugins_file = Some(self.directory.path().join("plugins.txt"));
        } else if self.selection.is_mo2() {
            config.mo2 = Some(mo2::Selection {
                instance_path: self.instance.clone(),
                profile: "Default".into(),
            });
        }
        config
    }

    /// Read published archive and raw-input provenance rather than inferring it from names.
    fn manifest(&self) -> serde_json::Value {
        read_manifest(&self.output)
    }

    /// Produce expected texture bytes through loose input, independent of archive matching.
    async fn byte_control(&self, source: &[u8]) -> Vec<u8> {
        let data = self.directory.path().join("ControlData");
        let output = self.directory.path().join("control");
        fs::create_dir_all(data.join("textures")).unwrap();
        fs::write(data.join("Skyrim.esm"), plugin_bytes(None, None)).unwrap();
        fs::write(
            data.join("Good.esp"),
            plugin_bytes(Some("Skyrim.esm"), Some(222.0)),
        )
        .unwrap();
        fs::write(data.join("textures/shared.dds"), source).unwrap();
        publish(cpu_config(&data, &output)).await;
        fs::read(output.join("textures/shared.ktx2")).unwrap()
    }
}

/// Build a native record header without importing game bytes or third-party plugin readers.
fn record(tag: &[u8; 4], form_id: u32, payload: &[u8]) -> Vec<u8> {
    [
        tag.as_slice(),
        &(payload.len() as u32).to_le_bytes(),
        &[0; 4],
        &form_id.to_le_bytes(),
        &[0, 0, 0, 0, 44, 0, 0, 0],
        payload,
    ]
    .concat()
}

/// Build a length-delimited native subrecord around synthetic payload bytes.
fn subrecord(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    [
        tag.as_slice(),
        &(payload.len() as u16).to_le_bytes(),
        payload,
    ]
    .concat()
}

/// Encode usable native headers with asymmetric healthy records and declared dependencies.
fn plugin_bytes(master: Option<&str>, setting: Option<f32>) -> Vec<u8> {
    let mut header = subrecord(
        b"HEDR",
        &[
            1.7f32.to_le_bytes(),
            u32::from(setting.is_some()).to_le_bytes(),
            0x801u32.to_le_bytes(),
        ]
        .concat(),
    );
    if let Some(master) = master {
        header.extend(subrecord(b"MAST", format!("{master}\0").as_bytes()));
        header.extend(subrecord(b"DATA", &[0; 8]));
    }
    let mut bytes = record(b"TES4", 0, &header);
    if let Some(value) = setting {
        bytes.extend(record(
            b"GMST",
            0x01000800,
            &[
                subrecord(b"EDID", b"fJumpHeightMin\0"),
                subrecord(b"DATA", &value.to_le_bytes()),
            ]
            .concat(),
        ));
    }
    bytes
}

/// Generate valid distinguishable DDS dimensions and pixels for competing archive sources.
fn texture(width: u32, seed: u64) -> Vec<u8> {
    dds::generate(
        &dds::Spec::new(dds::Format::X8R8G8B8, width, 4),
        &mut Rng::new(seed),
    )
    .unwrap()
}

/// Keep texture production identical in controls and archive/repair routes.
fn cpu_config(data: &Path, output: &Path) -> PipelineConfig {
    let mut config = PipelineConfig::new(data, output);
    config.no_lod = true;
    config.cpu_jobs = 1;
    config.io_jobs = 1;
    config.texture_encoder = TextureEncoder::Cpu;
    config
}

/// Drain progress through the real conversion and final publication path.
async fn publish(config: PipelineConfig) -> PipelineReport {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let result = AssetPipeline::run_async(config, tx).await;
    drain.await.unwrap();
    let report = result.unwrap();
    assert!(report.complete && report.skipped == 0, "{report:?}");
    report
}

/// Inspect actual published provenance, including staged repair manifests.
fn read_manifest(output: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap()).unwrap()
}

/// Check actual texture dimensions, exact control bytes and independent raw-source provenance.
fn assert_shared(output: &Path, expected: &[u8], raw: &[u8], width: u32) {
    let bytes = fs::read(output.join("textures/shared.ktx2")).unwrap();
    let header = ktx2::Reader::new(bytes.as_slice()).unwrap().header();
    assert_eq!(
        header.pixel_width, width,
        "wrong archive supplied shared DDS"
    );
    assert_eq!(header.pixel_height, 4);
    assert_eq!(
        hash_bytes(&bytes),
        hash_bytes(expected),
        "texture bytes differ from loose control"
    );
    let manifest = read_manifest(output);
    let recorded = manifest["entries"]["textures/shared.dds"]["source_hash"]
        .as_str()
        .unwrap();
    let raw_hash = hash_bytes(raw);
    assert!(
        recorded == raw_hash || recorded.starts_with(&format!("{raw_hash}:texture-encoding:")),
        "unexpected raw shared DDS source: {recorded}"
    );
}

/// Validate usable neighboring records and return recorded plugin provenance with handles closed.
fn database_provenance(output: &Path) -> Vec<(String, i64, String)> {
    let connection = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
    validate_database(&connection).unwrap();
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

/// A skipped direct owner must not become an unowned underscore/dash archive in either selection mode.
#[tokio::test]
async fn skipped_dash_and_underscore_archives_preserve_good_texture_bytes() {
    for selection in [Selection::Automatic, Selection::Player] {
        for suffix in ["_Textures", "-Textures"] {
            let fixture = Fixture::new(selection);
            fixture.plugin("Broken.esp", "Missing.esm", None);
            fixture.select(&["Good.esp", "Broken.esp"]);
            let good = texture(4, 11);
            let bad = texture(16, 47);
            let standalone = texture(12, 71);
            fixture.archive(
                "Good.bsa",
                &[
                    Entry::new("textures/shared.dds", &good),
                    Entry::new("textures/neighbor.dds", &good),
                ],
            );
            let rejected_archive = format!("Broken{suffix}.bsa");
            fixture.archive(
                &rejected_archive,
                &[
                    Entry::new("textures/shared.dds", &bad),
                    Entry::new("textures/rejected_only.dds", &bad),
                ],
            );
            fixture.archive(
                "Standalone.bsa",
                &[Entry::new("textures/standalone.dds", &standalone)],
            );
            let expected = fixture.byte_control(&good).await;
            publish(fixture.config()).await;
            assert_shared(&fixture.output, &expected, &good, 4);
            let names = database_provenance(&fixture.output);
            assert_eq!(
                names.iter().map(|row| row.0.as_str()).collect::<Vec<_>>(),
                ["Skyrim.esm", "Good.esp"]
            );
            assert!(
                fixture.manifest()["archives"]
                    .get(rejected_archive.to_ascii_lowercase())
                    .is_none()
            );
            assert!(!fixture.output.join("textures/rejected_only.ktx2").exists());
            assert!(fixture.output.join("textures/neighbor.ktx2").is_file());
            assert!(fixture.output.join("textures/standalone.ktx2").is_file());
        }
    }
}

/// Active MO2 underscore/dash companions now publish while genuinely unowned archives stay excluded.
#[tokio::test]
async fn active_mo2_dash_and_underscore_companions_publish_control_bytes() {
    for suffix in ["_Textures", "-Textures"] {
        let fixture = Fixture::new(Selection::Mo2Mod);
        let exact = texture(4, 11);
        let companion = texture(16, 47);
        fixture.archive("Good.bsa", &[Entry::new("textures/shared.dds", &exact)]);
        let active_archive = format!("Good{suffix}.bsa");
        fixture.archive(
            &active_archive,
            &[Entry::new("textures/shared.dds", &companion)],
        );
        fixture.archive(
            "Standalone.bsa",
            &[Entry::new("textures/standalone.dds", &texture(12, 71))],
        );
        let expected = fixture.byte_control(&companion).await;
        publish(fixture.config()).await;
        assert_shared(&fixture.output, &expected, &companion, 16);
        assert!(
            fixture.manifest()["archives"]
                .get(active_archive.to_ascii_lowercase())
                .is_some()
        );
        assert!(
            fixture.manifest()["archives"]
                .get("standalone.bsa")
                .is_none()
        );
        assert!(!fixture.output.join("textures/standalone.ktx2").exists());
        assert_eq!(database_provenance(&fixture.output).len(), 2);
    }
}

/// The longest installed name remains authoritative when its companion starts with dash/underscore.
#[tokio::test]
async fn recovered_longer_owner_cannot_be_claimed_by_an_active_shorter_name() {
    for selection in [
        Selection::Player,
        Selection::Mo2Mod,
        Selection::Mo2Overwrite,
    ] {
        for suffix in ["_Textures", "-Textures"] {
            let fixture = Fixture::new(selection);
            fixture.plugin("Good - Expansion.esp", "Missing.esm", None);
            fixture.select(&["Good.esp", "Good - Expansion.esp"]);
            let good = texture(4, 11);
            let bad = texture(16, 47);
            fixture.archive("Good.bsa", &[Entry::new("textures/shared.dds", &good)]);
            let rejected_archive = format!("Good - Expansion{suffix}.bsa");
            fixture.archive(
                &rejected_archive,
                &[Entry::new("textures/shared.dds", &bad)],
            );
            let expected = fixture.byte_control(&good).await;
            publish(fixture.config()).await;
            assert_shared(&fixture.output, &expected, &good, 4);
            assert!(
                fixture.manifest()["archives"]
                    .get(rejected_archive.to_ascii_lowercase())
                    .is_none()
            );
            assert_eq!(database_provenance(&fixture.output).len(), 2);
        }
    }
}

/// Real prior companion/cache admission cannot supply repaired bytes after the longer owner is recovered away.
#[tokio::test]
async fn warm_repair_rejects_cached_dash_and_underscore_companions() {
    for selection in [
        Selection::Player,
        Selection::Mo2Mod,
        Selection::Mo2Overwrite,
    ] {
        for suffix in ["_Textures", "-Textures"] {
            let fixture = Fixture::new(selection);
            fixture.plugin("Good - Expansion.esp", "Skyrim.esm", None);
            fixture.select(&["Good.esp", "Good - Expansion.esp"]);
            let good = texture(4, 11);
            let bad = texture(16, 47);
            fixture.archive(
                "Good.bsa",
                &[
                    Entry::new("textures/shared.dds", &good),
                    Entry::new("textures/neighbor.dds", &good),
                ],
            );
            let rejected_archive = format!("Good - Expansion{suffix}.bsa");
            fixture.archive(
                &rejected_archive,
                &[
                    Entry::new("textures/shared.dds", &bad),
                    Entry::new("textures/rejected_only.dds", &bad),
                ],
            );
            let expected = fixture.byte_control(&good).await;
            let config = fixture.config();
            publish(config.clone()).await;
            let original_plugins = database_provenance(&fixture.output);
            assert_eq!(original_plugins.len(), 3);
            let original_texture = fs::read(fixture.output.join("textures/shared.ktx2")).unwrap();
            assert_eq!(
                ktx2::Reader::new(original_texture.as_slice())
                    .unwrap()
                    .header()
                    .pixel_width,
                16
            );
            let manifest_path = fixture.output.join("conversion-manifest.json");
            let mut manifest: ConversionManifest =
                serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
            let entry = &manifest.archives[&rejected_archive.to_ascii_lowercase()];
            let cached_shared = entry
                .files
                .iter()
                .find(|file| file.path == "textures/shared.dds")
                .unwrap();
            assert_eq!(cached_shared.hash, hash_bytes(&bad));
            let blob = config
                .ingestion_cache_dir()
                .join(".ingestion-cache/sha256")
                .join(&cached_shared.hash[..2])
                .join(&cached_shared.hash);
            assert_eq!(fs::read(&blob).unwrap(), bad);
            fixture.plugin("Good - Expansion.esp", "Missing.esm", None);
            manifest.failures.insert(
                "textures/shared.dds".into(),
                "synthetic retry after owner recovery".into(),
            );
            manifest.complete = false;
            manifest.save(&manifest_path).unwrap();
            let original_manifest = fs::read(&manifest_path).unwrap();
            let original_database = fs::read(fixture.output.join("skyrim_world.db")).unwrap();
            let neighbor = hash_file(&fixture.output.join("textures/neighbor.ktx2")).unwrap();
            let unrelated = hash_file(&fixture.output.join("textures/rejected_only.ktx2")).unwrap();
            let preview = repair_failed(&config, false).unwrap();
            assert!(!preview.published && preview.failures.is_empty());
            assert_eq!(preview.converted, 1);
            assert_shared(&preview.directory.join("assets"), &expected, &good, 4);
            assert_eq!(fs::read(&manifest_path).unwrap(), original_manifest);
            assert_eq!(
                fs::read(fixture.output.join("textures/shared.ktx2")).unwrap(),
                original_texture
            );
            assert_eq!(
                fs::read(fixture.output.join("skyrim_world.db")).unwrap(),
                original_database
            );
            let applied = repair_failed(&config, true).unwrap();
            assert!(applied.published && applied.failures.is_empty());
            assert_eq!(applied.converted, 1);
            assert_shared(&fixture.output, &expected, &good, 4);
            assert_eq!(database_provenance(&fixture.output), original_plugins);
            assert_eq!(
                hash_file(&fixture.output.join("textures/neighbor.ktx2")).unwrap(),
                neighbor
            );
            assert_eq!(
                hash_file(&fixture.output.join("textures/rejected_only.ktx2")).unwrap(),
                unrelated
            );
        }
    }
}

/// Read a validated world origin without reproducing archive ownership or settings selection.
fn world_origin(output: &Path) -> (i32, i32) {
    let connection = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
    validate_database(&connection).unwrap();
    connection
        .query_row(
            "SELECT lod_origin_x,lod_origin_y FROM worldspaces WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

/// Metadata cannot assign a rejected longer owner's dash/underscore settings to the matched Skyrim prefix.
#[tokio::test]
async fn metadata_rebuild_rejects_dash_and_underscore_longer_owner_settings() {
    for suffix in ["_Textures", "-Textures"] {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let source = directory.path().join("source");
        let output = directory.path().join("derived");
        fs::write(directory.path().join("Skyrim.ccc"), b"").unwrap();
        layout::prepare_directory(&data, false).unwrap();
        layout::generate(
            &data,
            layout::DEFAULT_SEED,
            layout::Formats::parse("dds,nif,esm,lodsettings").unwrap(),
        )
        .unwrap();
        fs::remove_file(data.join("lodsettings/GeneratedWorld.lod")).unwrap();
        fs::write(
            data.join("Skyrim - ZBroken.esp"),
            plugin_bytes(Some("Missing.esm"), None),
        )
        .unwrap();
        let good_settings: [u8; 16] =
            [0xfc, 0xff, 0xfc, 0xff, 32, 0, 0, 0, 4, 0, 0, 0, 32, 0, 0, 0];
        fs::write(
            data.join("Skyrim - Misc.bsa"),
            bsa::v105(
                &[Entry::new("lodsettings/GeneratedWorld.lod", &good_settings)],
                bsa::Compression::None,
            )
            .unwrap(),
        )
        .unwrap();
        let mut config = cpu_config(&data, &source);
        config.no_lod = false;
        publish(config).await;
        assert_eq!(world_origin(&source), (-4, -4));
        let original_manifest = fs::read(source.join("conversion-manifest.json")).unwrap();
        let original_database = hash_file(&source.join("skyrim_world.db")).unwrap();
        let manifest: ConversionManifest = serde_json::from_slice(&original_manifest).unwrap();
        let mut bad_settings = good_settings;
        bad_settings[..2].copy_from_slice(&(-8i16).to_le_bytes());
        bad_settings[2..4].copy_from_slice(&(-8i16).to_le_bytes());
        fs::write(
            data.join(format!("Skyrim - ZBroken{suffix}.bsa")),
            bsa::v105(
                &[Entry::new("lodsettings/GeneratedWorld.lod", &bad_settings)],
                bsa::Compression::None,
            )
            .unwrap(),
        )
        .unwrap();
        let mut config = cpu_config(&data, &output);
        config.no_lod = false;
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let result = AssetPipeline::rebuild_metadata_async(config, &source, tx).await;
        drain.await.unwrap();
        let report = result.unwrap();
        assert!(report.complete && report.lod_chunks > 0);
        assert_eq!(world_origin(&output), (-4, -4));
        for entry in manifest.entries.values() {
            assert_eq!(
                hash_file(&output.join(&entry.output)).unwrap(),
                entry.output_hash
            );
        }
        assert_eq!(
            fs::read(source.join("conversion-manifest.json")).unwrap(),
            original_manifest
        );
        assert_eq!(
            hash_file(&source.join("skyrim_world.db")).unwrap(),
            original_database
        );
    }
}
