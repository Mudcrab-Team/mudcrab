//! Independent physical-slot regressions through both public pack-publication readers.
//!
//! xEdit TES5 definitions identify TX06 as multilayer, TX07 as backlight/specular,
//! and ARMO MOD2/MOD4 as its male/female world models. MOD3/MOD5 below are
//! deliberate synthetic decoys, not valid ARMO world-model fields.

mod common;

use converter::{
    AssetPipeline, PipelineConfig,
    config::RecordReader,
    esm::types::ArchivedRecordData,
    texture::{TextureEncoding, inspect_runtime_ktx2},
};
use dummy_content::{dds, inhouse_items as native, layout, rng::Rng};
use rusqlite::Connection;
use std::{
    fs,
    path::{Path, PathBuf},
};

const FULL_SLOT_PATHS: [&str; 8] = [
    "textures/rv_c_full_tx00.dds",
    "textures/rv_c_full_tx01.dds",
    "textures/rv_c_full_tx02.dds",
    "textures/rv_c_full_tx03.dds",
    "textures/rv_c_full_tx04.dds",
    "textures/rv_c_full_tx05.dds",
    "textures/rv_c_full_tx06.dds",
    "textures/rv_c_full_tx07.dds",
];
const LIGHT_SLOT_PATHS: [&str; 8] = [
    "textures/rv_c_light_tx00.dds",
    "textures/rv_c_light_tx01.dds",
    "textures/rv_c_light_tx02.dds",
    "textures/rv_c_light_tx03.dds",
    "textures/rv_c_light_tx04.dds",
    "textures/rv_c_light_tx05.dds",
    "textures/rv_c_light_tx06.dds",
    "textures/rv_c_light_tx07.dds",
];
const ARMOR_PATHS: [&str; 4] = [
    "meshes/rv_c_mod2_male_world.nif",
    "meshes/rv_c_mod3_decoy.nif",
    "meshes/rv_c_mod4_female_world.nif",
    "meshes/rv_c_mod5_decoy.nif",
];

/// Own synthetic inputs until both published packs have been inspected.
struct Fixture {
    directory: tempfile::TempDir,
    data: PathBuf,
    order: PathBuf,
    plugins: Vec<String>,
}

impl Fixture {
    /// Reserve unequal native/global full slots and a nonzero light slot.
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        layout::prepare_directory(&data, false).unwrap();
        layout::generate(
            &data,
            layout::DEFAULT_SEED,
            layout::Formats::parse("dds,nif,pex,esm").unwrap(),
        )
        .unwrap();
        let order = directory.path().join("plugins.txt");
        let mut fixture = Self {
            directory,
            data,
            order,
            plugins: vec!["Skyrim.esm".to_owned()],
        };
        fixture.add("FullFiller.esm", 0, &[]);
        fixture.add("LightFiller.esl", 0x200, &[]);
        fixture
    }

    /// Author a dependent plugin whose native self index is one, independently of its slot.
    fn add(&mut self, name: &str, flags: u32, records: &[Vec<u8>]) {
        let generated = fs::read(self.data.join("Skyrim.esm")).unwrap();
        let mut bytes = native::dependent_plugin(&generated, &["Skyrim.esm"], records);
        bytes[8..12].copy_from_slice(&flags.to_le_bytes());
        fs::write(self.data.join(name), bytes).unwrap();
        self.plugins.push(name.to_owned());
    }

    /// Supply every authored texture, including a cubemap in physical TX05.
    fn textures(&self, paths: &[&str; 8]) {
        let mut rng = Rng::new(0xA107);
        for (slot, path) in paths.iter().enumerate() {
            let spec = dds::Spec::new(dds::Format::Bc1Unorm, 4, 4);
            let spec = if slot == 5 { spec.as_cubemap() } else { spec };
            let bytes = if slot == 7 {
                // Unequal paths with identical source bytes isolate role-dependent encoding.
                fs::read(self.data.join(paths[6])).unwrap()
            } else {
                dds::generate(&spec, &mut rng).unwrap()
            };
            fs::write(self.data.join(path), bytes).unwrap();
        }
    }

    /// Give world models and decoys valid native assets so projection errors reach assertions.
    fn models(&self) {
        let bytes = fs::read(self.data.join(layout::GENERATED_MODEL_PATH)).unwrap();
        for path in ARMOR_PATHS {
            fs::write(self.data.join(path), &bytes).unwrap();
        }
    }

    /// Run the public converter through final publication with an explicit order and no LOD.
    async fn publish(&self, reader: RecordReader, reader_warnings: bool) -> PathBuf {
        let order: String = self
            .plugins
            .iter()
            .map(|name| format!("*{name}\n"))
            .collect();
        fs::write(&self.order, order).unwrap();
        let output = self.directory.path().join(match reader {
            RecordReader::Legacy => "legacy",
            RecordReader::Inhouse => "inhouse",
        });
        let mut config = PipelineConfig::new(&self.data, &output);
        config.plugins_file = Some(self.order.clone());
        config.record_reader = reader;
        config.no_lod = true;
        config.cpu_jobs = 2;
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let report = AssetPipeline::run_async(config, tx).await.unwrap();
        drain.await.unwrap();
        if reader_warnings {
            common::assert_reader_warning_publication(&report, &output);
        } else {
            assert!(report.complete, "{reader:?}: {report:?}");
        }
        for artifact in [
            "skyrim_world.db",
            "conversion-manifest.json",
            "cell_cache.rkyv",
        ] {
            assert!(output.join(artifact).is_file(), "unpublished {artifact}");
        }
        output
    }
}

/// Frame physical texture signatures without consulting either reader's projection mapping.
fn texture_set(id: u32, editor_id: &str, paths: [Option<&str>; 8], flags: &[u8]) -> Vec<u8> {
    let mut fields: Vec<(&[u8; 4], Vec<u8>)> =
        vec![(b"EDID", native::text(editor_id)), (b"OBND", vec![0; 12])];
    for (tag, path) in [
        b"TX00", b"TX01", b"TX02", b"TX03", b"TX04", b"TX05", b"TX06", b"TX07",
    ]
    .into_iter()
    .zip(paths)
    {
        if let Some(path) = path {
            fields.push((tag, native::text(path)));
        }
    }
    fields.push((b"DNAM", flags.to_vec()));
    native::record(b"TXST", id, &fields)
}

/// Retain the editor ID beside the eight named runtime columns.
#[derive(Debug, PartialEq, Eq)]
struct TextureRow {
    editor_id: String,
    paths: Vec<Option<String>>,
}

/// Select the precise remapped ID rather than whichever generated row has a populated slot.
fn texture_row(database: &Connection, id: u32) -> TextureRow {
    database
        .query_row(
            "SELECT editor_id,diffuse_path,normal_path,mask_path,glow_path,height_path,environment_path,specular_path,detail_path FROM texture_sets WHERE id=?1",
            [id],
            |row| {
                Ok(TextureRow {
                    editor_id: row.get(0)?,
                    paths: (1..9).map(|column| row.get(column)).collect::<rusqlite::Result<_>>()?,
                })
            },
        )
        .unwrap()
}

/// State the expected semantics directly: specular uses TX07 and detail uses TX06.
fn expected_texture_row(editor_id: &str, physical: [Option<&str>; 8]) -> TextureRow {
    TextureRow {
        editor_id: editor_id.to_owned(),
        paths: [
            physical[0],
            physical[1],
            physical[2],
            physical[3],
            physical[4],
            physical[5],
            physical[7],
            physical[6],
        ]
        .into_iter()
        .map(|path| path.map(str::to_owned))
        .collect(),
    }
}

/// Confirm that different native/full/light IDs reached their independently selected owners.
fn assert_owner(database: &Connection, id: u32, plugin: &str, local: u32) {
    let stored: (String, u32) = database
        .query_row(
            "SELECT plugin_name,internal_id FROM formid_map WHERE form_id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(stored, (plugin.to_owned(), local));
}

/// Generate identical native sources for the separate SQL regressions and asset-byte control.
fn texture_fixture() -> Fixture {
    let mut fixture = Fixture::new();
    fixture.textures(&FULL_SLOT_PATHS);
    fixture.textures(&LIGHT_SLOT_PATHS);
    fixture.add(
        "FullSlots.esp",
        0,
        &[texture_set(
            0x0100_08D1,
            "FullPhysicalSlots",
            FULL_SLOT_PATHS.map(Some),
            &[0, 0],
        )],
    );
    fixture.add(
        "LightSlots.esp",
        0x200,
        &[texture_set(
            0x0100_0AE3,
            "LightPhysicalSlots",
            LIGHT_SLOT_PATHS.map(Some),
            &[0, 0],
        )],
    );
    fixture
}

/// Every distinct physical TX00-TX07 slot reaches the same correct column in both readers.
#[tokio::test]
async fn rv_c_txst_all_physical_slots_publish_identically_from_both_readers() {
    let fixture = texture_fixture();
    let mut published = Vec::new();
    for reader in [RecordReader::Inhouse, RecordReader::Legacy] {
        let output = fixture.publish(reader, false).await;
        let database = Connection::open(output.join("skyrim_world.db")).unwrap();
        let rows = [
            texture_row(&database, 0x0200_08D1),
            texture_row(&database, 0xFE00_1AE3),
        ];
        assert_eq!(
            rows,
            [
                expected_texture_row("FullPhysicalSlots", FULL_SLOT_PATHS.map(Some)),
                expected_texture_row("LightPhysicalSlots", LIGHT_SLOT_PATHS.map(Some)),
            ],
            "{reader:?} physical-slot meaning",
        );
        assert_owner(&database, 0x0200_08D1, "fullslots.esp", 0x08D1);
        assert_owner(&database, 0xFE00_1AE3, "lightslots.esp", 0x0AE3);
        published.push(rows);
    }
    assert_eq!(published[0], published[1], "reader-dependent TXST columns");
}

/// Slot-role correction preserves full runtime bytes; this control also runs before the fix.
#[tokio::test]
async fn rv_c_reader_slot_projection_keeps_runtime_asset_bytes_identical() {
    let fixture = texture_fixture();
    for paths in [FULL_SLOT_PATHS, LIGHT_SLOT_PATHS] {
        assert_ne!(paths[6], paths[7]);
        assert_eq!(
            fs::read(fixture.data.join(paths[6])).unwrap(),
            fs::read(fixture.data.join(paths[7])).unwrap(),
            "TX06/TX07 must have identical DDS sources at distinct paths",
        );
    }
    // No expected SQL assertion gates this separate baseline/after-fix byte control.
    let inhouse = fixture.publish(RecordReader::Inhouse, false).await;
    let legacy = fixture.publish(RecordReader::Legacy, false).await;
    for paths in [FULL_SLOT_PATHS, LIGHT_SLOT_PATHS] {
        for path in paths {
            let runtime = Path::new(path).with_extension("ktx2");
            assert_eq!(
                fs::read(inhouse.join(&runtime)).unwrap(),
                fs::read(legacy.join(&runtime)).unwrap(),
                "reader-dependent complete KTX2 bytes: {path}",
            );
        }
        for output in [&inhouse, &legacy] {
            let multilayer =
                fs::read(output.join(Path::new(paths[6]).with_extension("ktx2"))).unwrap();
            let specular =
                fs::read(output.join(Path::new(paths[7]).with_extension("ktx2"))).unwrap();
            assert_eq!(multilayer, specular, "TX06/TX07 encoding differs");
            assert_eq!(
                inspect_runtime_ktx2(&multilayer).unwrap().encoding,
                TextureEncoding::ColorSrgb,
            );
            assert_eq!(
                inspect_runtime_ktx2(&specular).unwrap().encoding,
                TextureEncoding::ColorSrgb,
            );
        }
    }
    let model = Path::new(layout::GENERATED_MODEL_PATH).with_extension("glb");
    assert_eq!(
        fs::read(inhouse.join(&model)).unwrap(),
        fs::read(legacy.join(&model)).unwrap(),
        "reader-dependent complete GLB bytes from the same synthetic NIF",
    );
}

/// Author real world-model fields and separate synthetic MOD3/MOD5 decoys in schema order.
fn armor(id: u32, editor_id: &str, paths: [Option<&str>; 4]) -> Vec<u8> {
    let mut fields: Vec<(&[u8; 4], Vec<u8>)> =
        vec![(b"EDID", native::text(editor_id)), (b"OBND", vec![0; 12])];
    for (tag, path) in [b"MOD2", b"MOD3", b"MOD4", b"MOD5"].into_iter().zip(paths) {
        if let Some(path) = path {
            fields.push((tag, native::text(path)));
        }
    }
    fields.push((
        b"BOD2",
        [0x0040_0000u32.to_le_bytes(), 1u32.to_le_bytes()].concat(),
    ));
    fields.push((
        b"DATA",
        [197i32.to_le_bytes(), 3.625f32.to_le_bytes()].concat(),
    ));
    fields.push((b"DNAM", 731i32.to_le_bytes().to_vec()));
    native::record(b"ARMO", id, &fields)
}

/// ARMO chooses MOD2 first, MOD4 as fallback, and neither synthetic nonworld decoy.
#[tokio::test]
async fn rv_c_armo_world_model_slots_publish_identically_from_both_readers() {
    let mut fixture = Fixture::new();
    fixture.models();
    fixture.add(
        "ArmorSlots.esp",
        0,
        &[
            armor(0x0100_0911, "MaleWorldPreferred", ARMOR_PATHS.map(Some)),
            armor(
                0x0100_0912,
                "FemaleWorldFallback",
                [
                    None,
                    Some(ARMOR_PATHS[1]),
                    Some(ARMOR_PATHS[2]),
                    Some(ARMOR_PATHS[3]),
                ],
            ),
            armor(
                0x0100_0913,
                "NoWorldModel",
                [None, Some(ARMOR_PATHS[1]), None, Some(ARMOR_PATHS[3])],
            ),
            armor(
                0x0100_0914,
                "FemaleWorldOnly",
                [None, None, Some(ARMOR_PATHS[2]), None],
            ),
        ],
    );
    let expected = [
        (
            "MaleWorldPreferred".to_owned(),
            Some(ARMOR_PATHS[0].to_owned()),
        ),
        (
            "FemaleWorldFallback".to_owned(),
            Some(ARMOR_PATHS[2].to_owned()),
        ),
        ("NoWorldModel".to_owned(), None),
        (
            "FemaleWorldOnly".to_owned(),
            Some(ARMOR_PATHS[2].to_owned()),
        ),
    ];
    let mut published = Vec::new();
    for reader in [RecordReader::Inhouse, RecordReader::Legacy] {
        let output = fixture
            .publish(reader, reader == RecordReader::Inhouse)
            .await;
        let database = Connection::open(output.join("skyrim_world.db")).unwrap();
        let rows = [0x0200_0911, 0x0200_0912, 0x0200_0913, 0x0200_0914].map(|id| {
            database
                .query_row(
                    "SELECT editor_id,model_path FROM statics WHERE id=?1",
                    [id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .unwrap()
        });
        assert_eq!(rows, expected, "{reader:?} ARMO world-model meaning");
        published.push(rows);
    }
    assert_eq!(published[0], published[1], "reader-dependent ARMO model");
}

/// Safely framed damaged optional flags preserve their TXST paths and both valid neighbors.
#[tokio::test]
async fn rv_c_damaged_optional_txst_flags_preserve_neighbors_and_publish() {
    let mut fixture = Fixture::new();
    let paths = [
        Some(layout::GENERATED_DIFFUSE_PATH),
        Some(layout::GENERATED_NORMAL_PATH),
        None,
        None,
        None,
        None,
        None,
        None,
    ];
    fixture.add(
        "DamagedSlots.esp",
        0,
        &[
            texture_set(0x0100_08D0, "TextureNeighborBefore", paths, &[0, 0]),
            // DNAM is a u16; width three damages only this optional field, not its boundary.
            texture_set(
                0x0100_08D1,
                "TextureDamagedFlags",
                paths,
                &[0x51, 0xA7, 0xD2],
            ),
            texture_set(0x0100_08D2, "TextureNeighborAfter", paths, &[0, 0]),
        ],
    );
    let original = fs::read(fixture.data.join("DamagedSlots.esp")).unwrap();
    let mut published = Vec::new();
    for reader in [RecordReader::Inhouse, RecordReader::Legacy] {
        let output = fixture
            .publish(reader, reader == RecordReader::Inhouse)
            .await;
        let database = Connection::open(output.join("skyrim_world.db")).unwrap();
        let mut rows = Vec::new();
        for (id, editor_id) in [
            (0x0200_08D0u32, "TextureNeighborBefore"),
            (0x0200_08D1, "TextureDamagedFlags"),
            (0x0200_08D2, "TextureNeighborAfter"),
        ] {
            let stored = texture_row(&database, id);
            assert_eq!(
                stored,
                expected_texture_row(editor_id, paths),
                "{reader:?} {id:08X}",
            );
            let count: u32 = database
                .query_row(
                    "SELECT count(*) FROM records WHERE form_id=?1",
                    [id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "lost usable neighboring record {id:08X}");
            rows.push(stored);
        }
        if reader == RecordReader::Inhouse {
            let blob: Vec<u8> = database
                .query_row(
                    "SELECT data FROM records WHERE form_id=?1",
                    [0x0200_08D1u32],
                    |row| row.get(0),
                )
                .unwrap();
            let canonical =
                rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&blob).unwrap();
            assert!(
                !canonical
                    .subrecords
                    .iter()
                    .any(|field| field.tag == *b"DNAM")
            );
            assert_eq!(
                canonical
                    .subrecords
                    .iter()
                    .find(|field| field.tag == *b"TX00")
                    .unwrap()
                    .data,
                native::text(layout::GENERATED_DIFFUSE_PATH),
            );
            let diagnostics: serde_json::Value = serde_json::from_slice(
                &fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap(),
            )
            .unwrap();
            let damaged = &diagnostics["decoder"]["damagedslots.esp"];
            assert_eq!(damaged["skipped_records"], 0);
            assert_eq!(damaged["skipped_fields"], 1);
            let example = damaged["first_by_category"]["field"].as_str().unwrap();
            for expected in ["TXST", "020008D1", "DNAM"] {
                assert!(
                    example.contains(expected),
                    "missing skipped-field identity: {example}",
                );
            }
        }
        assert_eq!(
            fs::read(fixture.data.join("DamagedSlots.esp")).unwrap(),
            original,
        );
        published.push(rows);
    }
    assert_eq!(
        published[0], published[1],
        "damaged optional flags changed path projection",
    );
}
