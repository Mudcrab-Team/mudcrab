//! Selected-language and MO2 winners must survive both reader and ingestion routes.
use converter::{
    AssetPipeline, PipelineConfig, PipelineReport, TextureEncoder, archive::IngestionSync,
    config::RecordReader,
};
use dummy_content::{Entry, ba2, esm, inhouse_dialogue as native, layout};
use rusqlite::Connection;
use std::{fs, path::Path};

const NPC_ID: u32 = 0x0100_0800;

fn plugins(data: &Path, localized_root: &Path) {
    fs::create_dir_all(data).unwrap();
    fs::create_dir_all(localized_root).unwrap();
    let generated = esm::plugin(&esm::Plugin {
        author: "LocalizationIntegration",
        worldspace: layout::GENERATED_WORLDSPACE,
        cells: &[esm::PRESET_EXTERIOR_CELL],
        model_path: layout::GENERATED_MODEL_PATH,
        diffuse: layout::GENERATED_DIFFUSE_PATH,
        normal_texture: layout::GENERATED_NORMAL_PATH,
    })
    .unwrap();
    fs::write(
        data.join("Skyrim.esm"),
        native::plugin(&generated, &[], 1, &[]),
    )
    .unwrap();
    let npc = native::named(
        b"NPC_",
        NPC_ID,
        &[native::subrecord(b"FULL", &1u32.to_le_bytes())],
    );
    fs::write(
        localized_root.join("Names.esp"),
        native::plugin(&generated, &["Skyrim.esm"], 0x80, &npc),
    )
    .unwrap();
}

fn table(text: &str) -> Vec<u8> {
    let data = native::text(text);
    [
        1u32.to_le_bytes().as_slice(),
        &(data.len() as u32).to_le_bytes(),
        &1u32.to_le_bytes(),
        &0u32.to_le_bytes(),
        &data,
    ]
    .concat()
}

fn loose(root: &Path, language: &str, text: &str) {
    fs::create_dir_all(root.join("Strings")).unwrap();
    fs::write(
        root.join(format!("Strings/Names_{language}.STRINGS")),
        table(text),
    )
    .unwrap();
}

fn archive(data: &Path, french: &str) {
    let english = table("Archive English");
    let french = table(french);
    fs::write(
        data.join("Names.ba2"),
        ba2::general(
            &[
                Entry {
                    name: "Strings/Names_English.STRINGS",
                    data: &english,
                },
                Entry {
                    name: "Strings/Names_French.STRINGS",
                    data: &french,
                },
            ],
            ba2::Compression::None,
        )
        .unwrap(),
    )
    .unwrap();
}

fn config(data: &Path, output: &Path, reader: RecordReader) -> PipelineConfig {
    let mut config = PipelineConfig::new(data, output);
    config.record_reader = reader;
    config.texture_encoder = TextureEncoder::Cpu;
    config.lod_texture_encoder = TextureEncoder::Cpu;
    config.cpu_jobs = 2;
    config.no_lod = true;
    config
}

async fn run(config: &PipelineConfig, expected: Option<&str>, hit: bool) -> PipelineReport {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config.clone(), tx).await.unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    assert_eq!(report.database_cache_hit, hit, "{report:?}");
    let name: Option<String> = Connection::open(config.output_dir.join("skyrim_world.db"))
        .unwrap()
        .query_row("SELECT full_name FROM npcs WHERE id=?1", [NPC_ID], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(name.as_deref(), expected);
    report
}

#[tokio::test]
async fn selected_language_banks_invalidate_both_readers_and_both_ingestion_routes() {
    for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
        for sync in [IngestionSync::PerFile, IngestionSync::Archive] {
            let directory = tempfile::tempdir().unwrap();
            let data = directory.path().join("Data");
            plugins(&data, &data);
            archive(&data, "Archive French");
            let order = directory.path().join("plugins.txt");
            fs::write(&order, "*Skyrim.esm\n*Names.esp\n").unwrap();
            let mut config = config(&data, &directory.path().join("modern"), reader);
            config.plugins_file = Some(order);
            config.ingestion_sync = sync;

            let english = run(&config, Some("Archive English"), false).await;
            run(&config, Some("Archive English"), true).await;
            config.language = "FrEnCh".into();
            let french = run(&config, Some("Archive French"), false).await;
            assert_ne!(english.database_cache_key, french.database_cache_key);
            config.language = "french".into();
            run(&config, Some("Archive French"), true).await;
            archive(&data, "Edited Archive French");
            run(&config, Some("Edited Archive French"), false).await;
            loose(&data, "French", "Loose French");
            run(&config, Some("Loose French"), false).await;
            fs::remove_file(data.join("Strings/Names_French.STRINGS")).unwrap();
            run(&config, Some("Edited Archive French"), false).await;
            config.language = "spanish".into();
            run(&config, None, false).await;
            run(&config, None, true).await;
            loose(&data, "Spanish", "Added Spanish");
            run(&config, Some("Added Spanish"), false).await;
        }
    }
}

#[tokio::test]
async fn mo2_banks_follow_virtual_winners_instead_of_the_plugin_directory() {
    for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let instance = directory.path().join("MO2");
        let low = instance.join("mods/Low");
        let high = instance.join("mods/High");
        let overwrite = instance.join("overwrite");
        let profile = instance.join("profiles/Default");
        for path in [&low, &high, &overwrite, &profile] {
            fs::create_dir_all(path).unwrap();
        }
        plugins(&data, &low);
        loose(&data, "French", "Physical Data");
        loose(&low, "French", "Low Mod");
        loose(&high, "French", "High Mod");
        fs::write(
            instance.join("ModOrganizer.ini"),
            "[General]\ngameName=Skyrim Special Edition\n",
        )
        .unwrap();
        fs::write(profile.join("modlist.txt"), "+High\n+Low\n").unwrap();
        fs::write(profile.join("plugins.txt"), "*Names.esp\n").unwrap();
        fs::write(profile.join("loadorder.txt"), "Skyrim.esm\nNames.esp\n").unwrap();
        let mut config = config(&data, &directory.path().join("modern"), reader);
        config.language = "french".into();
        config.ingestion_sync = IngestionSync::Archive;
        config.mo2 = Some(mo2::Selection {
            instance_path: instance,
            profile: "Default".into(),
        });

        run(&config, Some("High Mod"), false).await;
        run(&config, Some("High Mod"), true).await;
        loose(&low, "French", "Edited Low Mod");
        run(&config, Some("High Mod"), true).await;
        loose(&high, "French", "Edited High Mod");
        run(&config, Some("Edited High Mod"), false).await;
        loose(&overwrite, "French", "Overwrite");
        run(&config, Some("Overwrite"), false).await;
        fs::remove_file(overwrite.join("Strings/Names_French.STRINGS")).unwrap();
        fs::write(profile.join("modlist.txt"), "-High\n+Low\n").unwrap();
        run(&config, Some("Edited Low Mod"), false).await;
    }
}
