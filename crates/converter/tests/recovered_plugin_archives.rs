//! Recovered plugin selections must also govern companion archives in real conversions.

use converter::{AssetPipeline, PipelineConfig, PipelineReport};
use dummy_content::{Entry, bsa, pex};
use std::{fs, path::PathBuf};

/// Keep each production conversion and its inputs isolated from other fixtures.
struct Fixture {
    _directory: tempfile::TempDir,
    data: PathBuf,
    output: PathBuf,
}

impl Fixture {
    /// Create an isolated Data tree and runtime-pack destination.
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let output = directory.path().join("pack");
        fs::create_dir_all(&data).unwrap();
        Self {
            _directory: directory,
            data,
            output,
        }
    }

    /// Write a complete TES4 header with optional master dependencies.
    fn plugin(&self, name: &str, masters: &[&str]) {
        let mut header = Vec::new();
        for master in masters {
            for (tag, payload) in [
                (b"MAST", format!("{master}\0").into_bytes()),
                (b"DATA", vec![0; 8]),
            ] {
                header.extend_from_slice(tag);
                header.extend_from_slice(&(payload.len() as u16).to_le_bytes());
                header.extend_from_slice(&payload);
            }
        }
        let bytes = [
            b"TES4".as_slice(),
            &(header.len() as u32).to_le_bytes(),
            &[0; 16],
            &header,
        ]
        .concat();
        let path = self.data.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    /// Put distinguishable, valid Papyrus bytecode at a shared or unique archive resource path.
    fn archive(&self, name: &str, resource: &str, object: &str) {
        self.archive_entries(name, &[(resource, object)]);
    }

    /// Store several distinguishable script resources in one companion archive.
    fn archive_entries(&self, name: &str, scripts: &[(&str, &str)]) {
        let payloads: Vec<_> = scripts
            .iter()
            .map(|(_, object)| pex::minimal(object).unwrap())
            .collect();
        let entries: Vec<_> = scripts
            .iter()
            .zip(&payloads)
            .map(|((resource, _), bytes)| Entry::new(resource, bytes))
            .collect();
        let archive = bsa::v105(&entries, bsa::Compression::None).unwrap();
        fs::write(self.data.join(name), archive).unwrap();
    }

    /// Select plugins explicitly, or leave selection to automatic discovery.
    fn config(&self, entries: Option<&[u8]>) -> PipelineConfig {
        let mut config = PipelineConfig::new(&self.data, &self.output);
        config.no_lod = true;
        config.cpu_jobs = 2;
        if let Some(entries) = entries {
            let list = self._directory.path().join("plugins.txt");
            fs::write(&list, entries).unwrap();
            config.plugins_file = Some(list);
        }
        config
    }

    /// Put mod inputs outside physical Data and resolve them through a real MO2 profile.
    fn mo2_config(&self, entries: &[u8]) -> (PipelineConfig, PathBuf) {
        let instance = self._directory.path().join("MO2");
        for folder in ["mods/Low", "mods/High", "profiles/Default", "overwrite"] {
            fs::create_dir_all(instance.join(folder)).unwrap();
        }
        fs::write(
            instance.join("ModOrganizer.ini"),
            "[General]\ngameName=Skyrim Special Edition\n",
        )
        .unwrap();
        fs::write(
            instance.join("profiles/Default/modlist.txt"),
            "+High\n+Low\n",
        )
        .unwrap();
        fs::write(instance.join("profiles/Default/plugins.txt"), entries).unwrap();
        for entry in fs::read_dir(&self.data).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name() != "Skyrim.esm" {
                fs::rename(
                    entry.path(),
                    instance.join("mods/Low").join(entry.file_name()),
                )
                .unwrap();
            }
        }
        let mut config = self.config(None);
        config.mo2 = Some(mo2::Selection {
            instance_path: instance.clone(),
            profile: "Default".into(),
        });
        (config, instance)
    }

    /// Inspect the distinguishable script shipped after archive overlay and publication.
    fn assert_script(&self, resource: &str, object: &str) {
        let target = PathBuf::from(resource).with_extension("luau");
        let script = fs::read_to_string(self.output.join(target)).unwrap();
        assert!(script.contains(object), "wrong published script: {script}");
    }

    /// Read the archive cache keys recorded in the published conversion manifest.
    fn archive_keys(&self) -> Vec<String> {
        let manifest: serde_json::Value = serde_json::from_slice(
            &fs::read(self.output.join("conversion-manifest.json")).unwrap(),
        )
        .unwrap();
        manifest["archives"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    }
}

/// Execute the production pipeline through extraction, conversion and publication.
async fn run(config: PipelineConfig) -> PipelineReport {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<converter::ProgressEvent>(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let result = AssetPipeline::run_async(config, tx).await;
    drain.await.unwrap();
    let report = result.unwrap();
    assert!(report.complete, "{report:?}");
    assert_eq!(report.skipped, 0, "{report:?}");
    report
}

/// An omitted mod and its dependent must not overwrite a surviving plugin's shared script.
async fn assert_recovered_archives(explicit: bool) {
    let fixture = Fixture::new();
    fixture.plugin("Skyrim.esm", &[]);
    fixture.plugin("Good.esp", &["Skyrim.esm"]);
    fixture.plugin("Broken.esp", &["Missing.esm"]);
    fixture.plugin("Child.esp", &["Broken.esp"]);
    fixture.archive("Good.bsa", "scripts/shared.pex", "GoodArchive");
    fixture.archive("Broken.bsa", "scripts/shared.pex", "BrokenArchive");
    fixture.archive("Child - Scripts.bsa", "scripts/shared.pex", "ChildArchive");
    let entries = explicit.then_some(b"*Good.esp\n*Broken.esp\n*Child.esp\n".as_slice());
    let report = run(fixture.config(entries)).await;
    assert!(
        report
            .notices
            .iter()
            .any(|notice| notice.starts_with("skipping Broken.esp:")
                && notice.contains("Missing.esm")),
        "{report:?}"
    );
    fixture.assert_script("scripts/shared.pex", "GoodArchive");
    assert_eq!(fixture.archive_keys(), ["good.bsa"]);
    let connection = rusqlite::Connection::open(fixture.output.join("skyrim_world.db")).unwrap();
    let plugins: Vec<String> = connection
        .prepare("SELECT name FROM plugins ORDER BY priority")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(plugins, ["Skyrim.esm", "Good.esp"]);
}

/// Explicit player-list recovery must exclude the missing-master mod's BSA from the asset winner.
#[tokio::test]
async fn explicit_recovery_excludes_skipped_plugin_archive_overrides() {
    assert_recovered_archives(true).await;
}

/// Automatic discovery must apply its recovered selection before archive extraction too.
#[tokio::test]
async fn discovered_recovery_excludes_skipped_plugin_archive_overrides() {
    assert_recovered_archives(false).await;
}

/// Installed unchecked plugins cannot contribute exact or suffixed companion archives.
#[tokio::test]
async fn explicit_selection_excludes_disabled_installed_companions() {
    let fixture = Fixture::new();
    fixture.plugin("Skyrim.esm", &[]);
    fixture.plugin("Good.esp", &["Skyrim.esm"]);
    fixture.plugin("Disabled.esp", &["Skyrim.esm"]);
    fixture.archive("Good.bsa", "scripts/shared.pex", "GoodArchive");
    fixture.archive("Disabled.bsa", "scripts/shared.pex", "DisabledArchive");
    fixture.archive(
        "Disabled - Scripts.bsa",
        "scripts/disabled.pex",
        "DisabledOnly",
    );
    run(fixture.config(Some(b"*Good.esp\nDisabled.esp\n"))).await;
    fixture.assert_script("scripts/shared.pex", "GoodArchive");
    assert!(!fixture.output.join("scripts/disabled.luau").exists());
    assert_eq!(fixture.archive_keys(), ["good.bsa"]);
}

/// A shorter active plugin name cannot activate a disabled plugin's more specific archive name.
#[tokio::test]
async fn most_specific_installed_owner_controls_archive_activation() {
    let fixture = Fixture::new();
    fixture.plugin("Skyrim.esm", &[]);
    fixture.plugin("Good.esp", &["Skyrim.esm"]);
    fixture.plugin("Good - Hidden.esp", &["Skyrim.esm"]);
    fixture.archive("Good - Scripts.bsa", "scripts/shared.pex", "GoodArchive");
    fixture.archive("Good - Hidden.bsa", "scripts/shared.pex", "HiddenArchive");
    fixture.archive(
        "Good - Hidden - Scripts.bsa",
        "scripts/hidden.pex",
        "HiddenOnly",
    );
    run(fixture.config(Some(b"*Good.esp\nGood - Hidden.esp\n"))).await;
    fixture.assert_script("scripts/shared.pex", "GoodArchive");
    assert!(!fixture.output.join("scripts/hidden.luau").exists());
    assert_eq!(fixture.archive_keys(), ["good - scripts.bsa"]);
}

/// Exact and spaced-dash companions retain case/Unicode matching and plugin load-order priority.
#[tokio::test]
async fn surviving_companions_preserve_load_order_and_unicode_matching() {
    let fixture = Fixture::new();
    fixture.plugin("Skyrim.esm", &[]);
    fixture.plugin("Good.esp", &["Skyrim.esm"]);
    fixture.plugin("CAFÉ.esp", &["Skyrim.esm"]);
    fixture.archive("GOOD.bsa", "scripts/exact.pex", "ExactArchive");
    fixture.archive("Good - Main.bsa", "scripts/shared.pex", "GoodMain");
    fixture.archive("Good - Textures.bsa", "scripts/shared.pex", "GoodTextures");
    fixture.archive("café - Scripts.bsa", "scripts/shared.pex", "UnicodeArchive");
    // The production player-list reader uses Windows-1252, not UTF-8.
    run(fixture.config(Some(b"*Good.esp\n*CAF\xC9.esp\n"))).await;
    let connection = rusqlite::Connection::open(fixture.output.join("skyrim_world.db")).unwrap();
    let plugins: Vec<String> = connection
        .prepare("SELECT name FROM plugins ORDER BY priority")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(plugins, ["Skyrim.esm", "Good.esp", "CAFÉ.esp"]);
    fixture.assert_script("scripts/exact.pex", "ExactArchive");
    fixture.assert_script("scripts/shared.pex", "UnicodeArchive");
    assert_eq!(fixture.archive_keys().len(), 4);
}

/// Unowned standalone archives and active base companions retain non-MO2 admission behavior.
#[tokio::test]
async fn non_mo2_selection_preserves_standalone_and_base_archives() {
    let fixture = Fixture::new();
    fixture.plugin("Skyrim.esm", &[]);
    fixture.plugin("Good.esp", &["Skyrim.esm"]);
    for (archive, resource, object) in [
        ("Skyrim - Misc.bsa", "scripts/base.pex", "BaseArchive"),
        (
            "Standalone.bsa",
            "scripts/standalone.pex",
            "StandaloneArchive",
        ),
        (
            "Good_Other.bsa",
            "scripts/underscore.pex",
            "UnderscoreArchive",
        ),
        ("Good Other.bsa", "scripts/space.pex", "SpaceArchive"),
        ("Good-Other.bsa", "scripts/dash.pex", "DashArchive"),
    ] {
        fixture.archive(archive, resource, object);
    }
    run(fixture.config(Some(b"*Good.esp\n"))).await;
    for (resource, object) in [
        ("scripts/base.pex", "BaseArchive"),
        ("scripts/standalone.pex", "StandaloneArchive"),
        ("scripts/underscore.pex", "UnderscoreArchive"),
        ("scripts/space.pex", "SpaceArchive"),
        ("scripts/dash.pex", "DashArchive"),
    ] {
        fixture.assert_script(resource, object);
    }
    assert_eq!(fixture.archive_keys().len(), 5);
}

/// Archive-only discovery still accepts base and standalone archives without any plugin owner.
#[tokio::test]
async fn archive_only_discovery_preserves_unowned_archives() {
    let fixture = Fixture::new();
    fixture.archive("Skyrim - Misc.bsa", "scripts/base.pex", "BaseArchive");
    fixture.archive(
        "Standalone.bsa",
        "scripts/standalone.pex",
        "StandaloneArchive",
    );
    run(fixture.config(None)).await;
    fixture.assert_script("scripts/base.pex", "BaseArchive");
    fixture.assert_script("scripts/standalone.pex", "StandaloneArchive");
    assert_eq!(fixture.archive_keys().len(), 2);
}

/// Ignored nested plugins cannot claim ownership of a standalone archive under automatic discovery.
#[tokio::test]
async fn ignored_nested_plugin_does_not_disable_unowned_archive() {
    let fixture = Fixture::new();
    fixture.plugin("Skyrim.esm", &[]);
    fixture.plugin("nested/Nested.esp", &["Missing.esm"]);
    fixture.archive("Nested.bsa", "scripts/standalone.pex", "StandaloneArchive");
    run(fixture.config(None)).await;
    fixture.assert_script("scripts/standalone.pex", "StandaloneArchive");
    assert_eq!(fixture.archive_keys(), ["nested.bsa"]);
}

/// A surviving short name cannot admit archives owned by rejected or unchecked longer MO2 names.
#[tokio::test]
async fn mo2_recovery_excludes_longer_owner_archives() {
    let fixture = Fixture::new();
    fixture.plugin("Skyrim.esm", &[]);
    fixture.plugin("Good.esp", &["Skyrim.esm"]);
    fixture.plugin("Good - Broken.esp", &["Missing.esm"]);
    fixture.plugin("Good - Disabled.esp", &["Skyrim.esm"]);
    fixture.archive("Good.bsa", "scripts/shared.pex", "GoodArchive");
    fixture.archive_entries(
        "Good - Broken.bsa",
        &[
            ("scripts/shared.pex", "RejectedOverride"),
            ("scripts/rejected.pex", "RejectedOnly"),
        ],
    );
    fixture.archive(
        "Good - Broken - Scripts.bsa",
        "scripts/rejected_companion.pex",
        "RejectedCompanion",
    );
    fixture.archive(
        "Good - Disabled.bsa",
        "scripts/disabled.pex",
        "DisabledOnly",
    );
    let (config, instance) =
        fixture.mo2_config(b"*Good.esp\n*Good - Broken.esp\nGood - Disabled.esp\n");
    // The VFS includes rejected and unchecked installed identities from enabled source layers.
    let resolved = mo2::Instance::open(&instance)
        .unwrap()
        .resolve(&fixture.data, "Default")
        .unwrap();
    for key in ["good - broken.esp", "good - disabled.esp"] {
        assert!(resolved.files.contains_key(key));
        assert!(!resolved.files[key].starts_with(&fixture.data));
    }
    let report = run(config).await;
    assert!(
        report.notices.iter().any(|notice| {
            notice.contains("skipping Good - Broken.esp:") && notice.contains("Missing.esm")
        }),
        "{report:?}"
    );
    assert!(!fixture.output.join("scripts/rejected.luau").exists());
    assert!(
        !fixture
            .output
            .join("scripts/rejected_companion.luau")
            .exists()
    );
    assert!(!fixture.output.join("scripts/disabled.luau").exists());
    fixture.assert_script("scripts/shared.pex", "GoodArchive");
    assert_eq!(fixture.archive_keys(), ["good.bsa"]);
}

/// Warm conversion must prune a recovered-away companion's archive and formerly shipped script.
#[tokio::test]
async fn mo2_warm_recovery_prunes_rejected_companion_scripts() {
    let fixture = Fixture::new();
    fixture.plugin("Skyrim.esm", &[]);
    fixture.plugin("Good.esp", &["Skyrim.esm"]);
    fixture.plugin("Good - Broken.esp", &["Skyrim.esm"]);
    fixture.archive("Good.bsa", "scripts/survivor.pex", "SurvivorArchive");
    fixture.archive(
        "Good - Broken.bsa",
        "scripts/rejected.pex",
        "PreviouslyActive",
    );
    let (config, instance) = fixture.mo2_config(b"*Good.esp\n*Good - Broken.esp\n");
    fs::create_dir_all(instance.join("overwrite/scripts")).unwrap();
    fs::write(
        instance.join("overwrite/scripts/loose.pex"),
        pex::minimal("LooseOverwrite").unwrap(),
    )
    .unwrap();
    run(config.clone()).await;
    fixture.assert_script("scripts/rejected.pex", "PreviouslyActive");
    fixture.assert_script("scripts/survivor.pex", "SurvivorArchive");
    fixture.assert_script("scripts/loose.pex", "LooseOverwrite");
    assert_eq!(fixture.archive_keys(), ["good - broken.bsa", "good.bsa"]);

    // Only the installed winning plugin header changes; configuration and BSA bytes are unchanged.
    fixture.plugin("Good - Broken.esp", &["Missing.esm"]);
    fs::rename(
        fixture.data.join("Good - Broken.esp"),
        instance.join("mods/Low/Good - Broken.esp"),
    )
    .unwrap();
    let report = run(config).await;
    assert!(
        report.notices.iter().any(|notice| {
            notice.contains("skipping Good - Broken.esp:") && notice.contains("Missing.esm")
        }),
        "{report:?}"
    );
    assert!(!fixture.output.join("scripts/rejected.luau").exists());
    fixture.assert_script("scripts/survivor.pex", "SurvivorArchive");
    fixture.assert_script("scripts/loose.pex", "LooseOverwrite");
    assert_eq!(fixture.archive_keys(), ["good.bsa"]);
}

/// MO2 keeps enabled-layer/overwrite priority, active Unicode companions and no-owner rejection.
#[tokio::test]
async fn mo2_active_companions_preserve_source_and_loose_priorities() {
    let fixture = Fixture::new();
    fixture.plugin("Skyrim.esm", &[]);
    fixture.plugin("Good.esp", &["Skyrim.esm"]);
    fixture.plugin("Good.esm", &["Skyrim.esm"]);
    fixture.plugin("CAFÉ.esp", &["Skyrim.esm"]);
    fixture.plugin("nested/Good - Nested.esp", &["Missing.esm"]);
    fixture.archive("Good.bsa", "scripts/source.pex", "LowArchive");
    fixture.archive("Good - Main.bsa", "scripts/main.pex", "ActiveMain");
    fixture.archive(
        "café - Textures.bsa",
        "scripts/unicode.pex",
        "UnicodeArchive",
    );
    fixture.archive(
        "Good - Nested.bsa",
        "scripts/nested.pex",
        "ActivePrefixCompanion",
    );
    fixture.archive("Standalone.bsa", "scripts/unowned.pex", "UnownedArchive");
    let (config, instance) = fixture.mo2_config(b"*Good.esp\nGood.esm\n*CAF\xC9.esp\n");
    let high_script = pex::minimal("HighArchive").unwrap();
    let archive = bsa::v105(
        &[Entry::new("scripts/source.pex", &high_script)],
        bsa::Compression::None,
    )
    .unwrap();
    fs::write(instance.join("mods/High/GOOD.bsa"), archive).unwrap();
    for (layer, object) in [("mods/Low", "LowLoose"), ("overwrite", "LooseOverwrite")] {
        fs::create_dir_all(instance.join(layer).join("scripts")).unwrap();
        fs::write(
            instance.join(layer).join("scripts/loose.pex"),
            pex::minimal(object).unwrap(),
        )
        .unwrap();
    }
    run(config).await;
    fixture.assert_script("scripts/source.pex", "HighArchive");
    fixture.assert_script("scripts/loose.pex", "LooseOverwrite");
    fixture.assert_script("scripts/main.pex", "ActiveMain");
    fixture.assert_script("scripts/unicode.pex", "UnicodeArchive");
    fixture.assert_script("scripts/nested.pex", "ActivePrefixCompanion");
    assert!(!fixture.output.join("scripts/unowned.luau").exists());
    assert_eq!(fixture.archive_keys().len(), 4);
}

/// An unchecked ESP peer cannot erase companion activation supplied by the same-stem ESM.
#[tokio::test]
async fn mo2_same_stem_active_master_keeps_companions() {
    let fixture = Fixture::new();
    fixture.plugin("Skyrim.esm", &[]);
    fixture.plugin("Good.esm", &["Skyrim.esm"]);
    fixture.plugin("Good.esp", &["Skyrim.esm"]);
    fixture.archive("Good.bsa", "scripts/exact.pex", "ActiveMaster");
    fixture.archive(
        "Good - Main.bsa",
        "scripts/main.pex",
        "ActiveMasterCompanion",
    );
    let (config, _) = fixture.mo2_config(b"*Good.esm\nGood.esp\n");
    run(config).await;
    fixture.assert_script("scripts/exact.pex", "ActiveMaster");
    fixture.assert_script("scripts/main.pex", "ActiveMasterCompanion");
    assert_eq!(fixture.archive_keys(), ["good - main.bsa", "good.bsa"]);
    let connection = rusqlite::Connection::open(fixture.output.join("skyrim_world.db")).unwrap();
    let plugins: Vec<String> = connection
        .prepare("SELECT name FROM plugins ORDER BY priority")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(plugins, ["Skyrim.esm", "Good.esm"]);
}
