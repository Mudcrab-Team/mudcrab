//! Shared Creation fallback through real player-list and MO2 database publication.

use converter::{AssetPipeline, PipelineConfig, PipelineReport, TextureEncoder};
use rusqlite::Connection;
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};

const OFFICIAL: [&str; 5] = [
    "Skyrim.esm",
    "Update.esm",
    "Dawnguard.esm",
    "HearthFires.esm",
    "Dragonborn.esm",
];
const FISH: &str = "ccBGSSSE001-Fish.esm";

#[derive(Clone, Copy)]
enum Source {
    Player,
    Mo2,
}

#[derive(Clone, Copy)]
enum Ccc {
    Missing,
    Unreadable,
    OtherCreation,
    Empty,
}

/// Isolate synthetic plugin sources, optional CCC input and published database.
struct Fixture {
    _directory: tempfile::TempDir,
    config: PipelineConfig,
    ccc_path: PathBuf,
}

impl Fixture {
    /// Install implicit masters, an inactive Fish, its dependent and a valid neighboring mod.
    fn new(source: Source, ccc: Ccc) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let data = root.join("Data");
        fs::create_dir(&data).unwrap();
        for name in OFFICIAL {
            fs::write(data.join(name), record(b"TES4", 0, 1, Vec::new())).unwrap();
        }
        plugin(&data.join(FISH), "Skyrim.esm", 1, "fCreationFish", 11.0);
        plugin(&data.join("Good.esp"), "Skyrim.esm", 0, "fGood", 29.0);
        plugin(
            &data.join("FishDependent.esp"),
            FISH,
            0,
            "fFishDependent",
            53.0,
        );
        plugin(
            &data.join("OtherCreation.esl"),
            "Skyrim.esm",
            0x200,
            "fOtherCreation",
            71.0,
        );
        // MO2 must retain case-insensitive optional-child resolution for this file.
        let ccc_path = root.join(match source {
            Source::Player => "Skyrim.ccc",
            Source::Mo2 if !matches!(ccc, Ccc::Missing) => "sKyRiM.CcC",
            Source::Mo2 => "Skyrim.ccc",
        });
        match ccc {
            Ccc::Missing => {}
            Ccc::Unreadable => fs::create_dir(&ccc_path).unwrap(),
            Ccc::OtherCreation => fs::write(&ccc_path, "OtherCreation.esl\n").unwrap(),
            Ccc::Empty => fs::write(&ccc_path, "# No active Creations\n").unwrap(),
        }
        let active = "*Good.esp\n*FishDependent.esp\n";
        let mut config = PipelineConfig::new(&data, root.join("pack"));
        match source {
            Source::Player => {
                let list = root.join("plugins.txt");
                fs::write(&list, active).unwrap();
                config.plugins_file = Some(list);
            }
            Source::Mo2 => {
                let instance = root.join("MO2");
                for folder in ["mods", "profiles/Default", "overwrite"] {
                    fs::create_dir_all(instance.join(folder)).unwrap();
                }
                fs::write(
                    instance.join("ModOrganizer.ini"),
                    "[General]\ngameName=Skyrim Special Edition\n",
                )
                .unwrap();
                fs::write(instance.join("profiles/Default/modlist.txt"), "").unwrap();
                fs::write(instance.join("profiles/Default/plugins.txt"), active).unwrap();
                fs::write(
                    instance.join("profiles/Default/loadorder.txt"),
                    "Good.esp\nFishDependent.esp\n",
                )
                .unwrap();
                config.mo2 = Some(mo2::Selection {
                    instance_path: instance,
                    profile: "Default".into(),
                });
            }
        }
        config.no_lod = true;
        config.cpu_jobs = 2;
        config.texture_encoder = TextureEncoder::Cpu;
        Self {
            _directory: directory,
            config,
            ccc_path,
        }
    }

    /// Publish through the real pipeline while draining its bounded progress channel.
    async fn publish(&self) -> PipelineReport {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<converter::ProgressEvent>(64);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let result = AssetPipeline::run_async(self.config.clone(), tx).await;
        drain.await.unwrap();
        assert!(
            result.is_ok(),
            "usable synthetic neighbors must publish: {result:?}"
        );
        let report = result.unwrap();
        assert!(report.complete && report.skipped == 0, "{report:?}");
        report
    }

    /// Read final slot-order provenance directly from the published plugin table.
    fn names(&self) -> Vec<String> {
        let conn = Connection::open(self.config.output_dir.join("skyrim_world.db")).unwrap();
        conn.prepare("SELECT name FROM plugins ORDER BY priority")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// Decode generic GMST rows so distinct neighbor and dependent payloads are both checked.
    fn settings(&self) -> Vec<(String, f32)> {
        let conn = Connection::open(self.config.output_dir.join("skyrim_world.db")).unwrap();
        conn.prepare("SELECT data FROM records WHERE record_type='GMST'")
            .unwrap()
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .unwrap()
            .map(|row| {
                let data = rkyv::from_bytes::<
                    converter::esm::types::ArchivedRecordData,
                    rkyv::rancor::Error,
                >(&row.unwrap())
                .unwrap();
                let editor = &data
                    .subrecords
                    .iter()
                    .find(|sub| sub.tag == *b"EDID")
                    .unwrap()
                    .data;
                let value = &data
                    .subrecords
                    .iter()
                    .find(|sub| sub.tag == *b"DATA")
                    .unwrap()
                    .data;
                (
                    String::from_utf8(editor.strip_suffix(&[0]).unwrap().to_vec()).unwrap(),
                    f32::from_le_bytes(value.as_slice().try_into().unwrap()),
                )
            })
            .collect()
    }

    /// Require one typed report diagnostic identifying the failed CCC input.
    fn assert_fallback_warning(&self, report: &PipelineReport) {
        // JSON access also compiles against the unfixed report schema for the causal red run.
        let report = serde_json::to_value(report).unwrap();
        let warnings = report["selection_warnings"].as_array();
        assert!(
            warnings.is_some(),
            "Creation fallback must have a report warning category: {report}"
        );
        let warnings = warnings.unwrap();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(
            warnings[0]["ccc_path"],
            serde_json::to_value(&self.ccc_path).unwrap()
        );
        assert!(
            warnings[0]["reason"]
                .as_str()
                .is_some_and(|reason| !reason.is_empty()),
            "fallback must explain why CCC could not be read: {warnings:?}"
        );
    }
}

/// Encode a synthetic subrecord with an independently sized payload.
fn sub(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    [tag.as_slice(), &(bytes.len() as u16).to_le_bytes(), bytes].concat()
}

/// Encode a synthetic Skyrim SE record header and payload.
fn record(tag: &[u8; 4], id: u32, flags: u32, payload: Vec<u8>) -> Vec<u8> {
    [
        tag.as_slice(),
        &(payload.len() as u32).to_le_bytes(),
        &flags.to_le_bytes(),
        &id.to_le_bytes(),
        &[0; 8],
        &payload,
    ]
    .concat()
}

/// Generate a plugin with a declared master and a distinguishable native-local GMST.
fn plugin(path: &Path, master: &str, flags: u32, editor: &str, value: f32) {
    let header = [
        sub(b"MAST", format!("{master}\0").as_bytes()),
        sub(b"DATA", &[0; 8]),
    ]
    .concat();
    let mut bytes = record(b"TES4", 0, flags, header);
    bytes.extend(record(
        b"GMST",
        0x0100_0800,
        0,
        [
            sub(b"EDID", format!("{editor}\0").as_bytes()),
            sub(b"DATA", &value.to_le_bytes()),
        ]
        .concat(),
    ));
    fs::write(path, bytes).unwrap();
}

/// Both fallback readers must activate Fish after officials and publish its selected dependent.
async fn assert_fallback(source: Source, ccc: Ccc) {
    let fixture = Fixture::new(source, ccc);
    let report = fixture.publish().await;
    let mut expected = OFFICIAL.into_iter().map(str::to_owned).collect::<Vec<_>>();
    expected.extend([FISH, "Good.esp", "FishDependent.esp"].map(str::to_owned));
    assert_eq!(
        fixture.names(),
        expected,
        "installed Fish must be implicitly active"
    );
    let settings = fixture.settings();
    for (editor, value) in [
        ("fGood", 29.0),
        ("fCreationFish", 11.0),
        ("fFishDependent", 53.0),
    ] {
        assert!(
            settings.contains(&(editor.to_owned(), value)),
            "{settings:?}"
        );
    }
    fixture.assert_fallback_warning(&report);
}

/// A readable CCC, including an empty file, must prevent built-in fallback activation.
async fn assert_authoritative(source: Source, ccc: Ccc) {
    let fixture = Fixture::new(source, ccc);
    let report = fixture.publish().await;
    let mut expected = OFFICIAL.into_iter().map(str::to_owned).collect::<Vec<_>>();
    if matches!(ccc, Ccc::OtherCreation) {
        expected.push("OtherCreation.esl".into());
    }
    expected.push("Good.esp".into());
    assert_eq!(fixture.names(), expected);
    let settings = fixture.settings();
    assert!(settings.contains(&("fGood".into(), 29.0)), "{settings:?}");
    assert!(
        !settings
            .iter()
            .any(|(editor, _)| editor == "fFishDependent" || editor == "fCreationFish")
    );
    if matches!(ccc, Ccc::OtherCreation) {
        assert!(
            settings.contains(&("fOtherCreation".into(), 71.0)),
            "{settings:?}"
        );
    }
    let report = serde_json::to_value(report).unwrap();
    assert!(
        report.get("selection_warnings").is_none_or(|warnings| {
            warnings == &Value::Null || warnings.as_array().is_some_and(Vec::is_empty)
        }),
        "readable CCC must not report fallback: {report}"
    );
}

/// A missing player CCC must activate installed Fish and preserve its dependent and neighbor.
#[tokio::test]
async fn player_missing_ccc_activates_installed_fish_and_dependent() {
    assert_fallback(Source::Player, Ccc::Missing).await;
}

/// An unreadable player CCC must use the same installed-Creation fallback.
#[tokio::test]
async fn player_unreadable_ccc_activates_installed_fish_and_dependent() {
    assert_fallback(Source::Player, Ccc::Unreadable).await;
}

/// A missing game-folder CCC must activate installed Fish in MO2 selection.
#[tokio::test]
async fn mo2_missing_ccc_activates_installed_fish_and_dependent() {
    assert_fallback(Source::Mo2, Ccc::Missing).await;
}

/// MO2 must resolve a mixed-case CCC filename and fall back when that input is unreadable.
#[tokio::test]
async fn mo2_unreadable_ccc_activates_installed_fish_and_dependent() {
    assert_fallback(Source::Mo2, Ccc::Unreadable).await;
}

/// A readable player CCC activates its authored Creation without adding installed Fish.
#[tokio::test]
async fn player_readable_ccc_is_authoritative() {
    assert_authoritative(Source::Player, Ccc::OtherCreation).await;
}

/// A readable mixed-case game-folder CCC remains authoritative for MO2.
#[tokio::test]
async fn mo2_readable_ccc_is_authoritative() {
    assert_authoritative(Source::Mo2, Ccc::OtherCreation).await;
}

/// An empty player CCC must keep every unlisted built-in Creation inactive.
#[tokio::test]
async fn player_readable_empty_ccc_is_authoritative() {
    assert_authoritative(Source::Player, Ccc::Empty).await;
}

/// An empty MO2 game-folder CCC must keep every unlisted built-in Creation inactive.
#[tokio::test]
async fn mo2_readable_empty_ccc_is_authoritative() {
    assert_authoritative(Source::Mo2, Ccc::Empty).await;
}

/// The player fallback must appear in the serialized report even before selection is inspected.
#[tokio::test]
async fn player_missing_ccc_reports_fallback_warning() {
    let fixture = Fixture::new(Source::Player, Ccc::Missing);
    let report = fixture.publish().await;
    assert!(fixture.settings().contains(&("fGood".into(), 29.0)));
    fixture.assert_fallback_warning(&report);
}

/// The MO2 fallback must appear in the serialized report even before selection is inspected.
#[tokio::test]
async fn mo2_missing_ccc_reports_fallback_warning() {
    let fixture = Fixture::new(Source::Mo2, Ccc::Missing);
    let report = fixture.publish().await;
    assert!(fixture.settings().contains(&("fGood".into(), 29.0)));
    fixture.assert_fallback_warning(&report);
}
