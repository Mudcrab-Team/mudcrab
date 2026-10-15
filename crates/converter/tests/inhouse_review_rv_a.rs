//! Independent native fixtures exercise RV-A recovery through complete publication.
mod common;

use converter::{
    AssetPipeline, PipelineConfig,
    config::RecordReader,
    esm::inhouse::{self, RecordBundleMode},
    records::{
        history::read_history,
        history_source::{SourceQuery, visit_source_history},
        source_identity::SourceIdentityState,
    },
    shared::{CellCache, TerrainLayer, TerrainWeight},
};
use dummy_content::layout;
use rusqlite::Connection;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

/// Independently frame one native subrecord, retaining authored payload bytes.
fn subrecord(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut bytes = tag.to_vec();
    bytes.extend(u16::try_from(payload.len()).unwrap().to_le_bytes());
    bytes.extend(payload);
    bytes
}

/// Independently frame a native header with revision and padding sentinels.
fn record(tag: &[u8; 4], source_id: u32, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut bytes = tag.to_vec();
    bytes.extend(u32::try_from(payload.len()).unwrap().to_le_bytes());
    bytes.extend(flags.to_le_bytes());
    bytes.extend(source_id.to_le_bytes());
    bytes.extend(0x5566_7788u32.to_le_bytes());
    bytes.extend(44u16.to_le_bytes());
    bytes.extend(0x1234u16.to_le_bytes());
    bytes.extend(payload);
    bytes
}

/// Emit a native group without using the converter's framing implementation.
fn group(kind: u32, label: u32, payload: &[u8]) -> Vec<u8> {
    let mut bytes = b"GRUP".to_vec();
    bytes.extend(u32::try_from(payload.len() + 24).unwrap().to_le_bytes());
    bytes.extend(label.to_le_bytes());
    bytes.extend(kind.to_le_bytes());
    bytes.extend([0; 8]);
    bytes.extend(payload);
    bytes
}

/// Encode authored fixture text as a native zero-terminated string.
fn text(value: &str) -> Vec<u8> {
    let mut bytes = value.as_bytes().to_vec();
    bytes.push(0);
    bytes
}

/// Build a usable static whose model has a generated asset beside it.
fn static_payload(editor_id: &str) -> Vec<u8> {
    let mut bytes = subrecord(b"EDID", &text(editor_id));
    bytes.extend(subrecord(b"MODL", &text(layout::GENERATED_MODEL_PATH)));
    bytes
}

/// Encode one complete zlib stream and its native uncompressed-length prefix.
fn compressed(payload: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(payload).unwrap();
    let mut bytes = u32::try_from(payload.len()).unwrap().to_le_bytes().to_vec();
    bytes.extend(encoder.finish().unwrap());
    bytes
}

/// Keep generated assets, original plugins and replay inputs alive for the entire test.
struct Fixture {
    directory: tempfile::TempDir,
    data: PathBuf,
    header: Vec<u8>,
    plugins: PathBuf,
}

impl Fixture {
    /// Generate a complete synthetic world without proprietary inputs.
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
        let base = fs::read(data.join("Skyrim.esm")).unwrap();
        let size = u32::from_le_bytes(base[4..8].try_into().unwrap()) as usize;
        let header = base[24..24 + size].to_vec();
        let plugins = directory.path().join("plugins.txt");
        Self {
            directory,
            data,
            header,
            plugins,
        }
    }

    /// Frame original declared masters with nonzero DATA padding.
    fn plugin_bytes(&self, masters: &[&str], flags: u32, body: &[u8]) -> Vec<u8> {
        let mut header = self.header.clone();
        for master in masters {
            header.extend(subrecord(b"MAST", &text(master)));
            header.extend(subrecord(b"DATA", &[0xA7; 8]));
        }
        let mut bytes = record(b"TES4", 0, flags, &header);
        bytes.extend(body);
        bytes
    }

    /// Write one test original and return its exact bytes for read-only assertions.
    fn plugin(&self, name: &str, masters: &[&str], flags: u32, body: &[u8]) -> Vec<u8> {
        let bytes = self.plugin_bytes(masters, flags, body);
        fs::write(self.data.join(name), &bytes).unwrap();
        bytes
    }

    /// Append test-owned native records beside the generated master's world.
    fn extend_base(&self, body: &[u8]) {
        let path = self.data.join("Skyrim.esm");
        let mut bytes = fs::read(&path).unwrap();
        bytes.extend(body);
        fs::write(path, bytes).unwrap();
    }

    /// Verify that native ordering retains the asymmetric fixture slots.
    fn order(&self, names: &[&str]) {
        fs::write(
            &self.plugins,
            names
                .iter()
                .map(|name| format!("*{name}\n"))
                .collect::<String>(),
        )
        .unwrap();
        let actual = converter::esm::read_plugins_txt(&self.plugins, &self.data).unwrap();
        let actual: Vec<_> = actual
            .iter()
            .map(|path| path.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(actual, names);
    }

    /// Publish real meshes, textures, database and both terrain caches with LOD disabled.
    async fn publish(&self) -> PathBuf {
        let output = self.directory.path().join("pack");
        let mut config = PipelineConfig::new(&self.data, &output);
        config.plugins_file = Some(self.plugins.clone());
        config.record_reader = RecordReader::Inhouse;
        config.no_lod = true;
        config.cpu_jobs = 2;
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let report = AssetPipeline::run_async(config, tx).await.unwrap();
        drain.await.unwrap();
        common::assert_reader_warning_publication(&report, &output);
        output
    }

    /// Exercise the focused public producer with the full runtime and cache contract.
    fn bundle(&self) -> PathBuf {
        let output = self.directory.path().join("bundle");
        inhouse::export_record_bundle_typed_with_options(
            &self.data,
            &self.plugins,
            &output,
            &[*b"STAT", *b"GMST", *b"LAND"],
            None,
            RecordBundleMode::Full,
        )
        .unwrap();
        output
    }
}

/// One independently authored deletion and its expected typed replay capability.
struct Deletion {
    runtime_id: u32,
    source_id: u32,
    offset: u64,
    frame: Vec<u8>,
    typed_decodable: bool,
}

/// Retained source originals bind deletion assertions to the supplying files.
struct DeletionCase {
    fixture: Fixture,
    deletions: Vec<Deletion>,
    owner_original: Vec<u8>,
    patch_original: Vec<u8>,
    retained_payload: Vec<u8>,
}

/// Slot-one originals and slot-zero master overrides expose actual FormID conversion.
fn deletion_case() -> DeletionCase {
    let fixture = Fixture::new();
    let torn = [subrecord(b"EDID", b"torn\0"), b"MODL\x10\x00bad".to_vec()].concat();
    let payloads = [
        (0x1800, vec![], false),
        (0x1801, vec![0xA7], false),
        (0x1802, vec![0xA7, 0x53, 0x19], false),
        (0x1803, vec![4, 0, 0, 0, 99], false),
        (
            0x1804,
            compressed(&static_payload("DeletedCompressed")),
            true,
        ),
        (0x1805, compressed(&torn), false),
    ];
    let mut owner = Vec::new();
    let mut patch = record(b"STAT", 0x0100_1841, 0, &static_payload("BeforeTombstones"));
    let body_offset = fixture.plugin_bytes(&["Owner.esm"], 0, &[]).len() as u64;
    let mut deletions = Vec::new();
    for (source_id, payload, typed_decodable) in payloads {
        owner.extend(record(b"STAT", source_id, 0, &static_payload("Original")));
        let frame = record(b"STAT", source_id, 0x40020, &payload);
        deletions.push(Deletion {
            runtime_id: 0x0100_0000 | source_id,
            source_id,
            offset: body_offset + patch.len() as u64,
            frame: frame.clone(),
            typed_decodable,
        });
        patch.extend(frame);
    }
    let retained_payload = static_payload("RetainedAfterBadCompression");
    owner.extend(record(b"STAT", 0x1820, 0, &retained_payload));
    patch.extend(record(b"STAT", 0x1820, 0x40000, &[4, 0, 0, 0, 99]));
    let mut setting = subrecord(b"EDID", b"fReviewCompressedSetting\0");
    setting.extend(subrecord(b"DATA", &17.25f32.to_le_bytes()));
    owner.extend(record(b"GMST", 0x1830, 0, &setting));
    // This new file-relative self ID can identify the predecessor only by its EDID alias.
    let frame = record(b"GMST", 0x0100_1840, 0x40020, &compressed(&setting));
    deletions.push(Deletion {
        runtime_id: 0x0100_1830,
        source_id: 0x0100_1840,
        offset: body_offset + patch.len() as u64,
        frame: frame.clone(),
        typed_decodable: true,
    });
    patch.extend(frame);
    patch.extend(record(
        b"STAT",
        0x0100_1842,
        0,
        &static_payload("AfterTombstones"),
    ));
    let owner_original = fixture.plugin("Owner.esm", &[], 1, &owner);
    let patch_original = fixture.plugin("Delete.esp", &["Owner.esm"], 0, &patch);
    fixture.order(&["Skyrim.esm", "Owner.esm", "Delete.esp"]);
    DeletionCase {
        fixture,
        deletions,
        owner_original,
        patch_original,
        retained_payload,
    }
}

/// Assert that an unusable candidate is absent from canonical and live source output.
fn assert_absent(db: &Connection, id: u32) {
    for sql in [
        "SELECT count(*) FROM records WHERE form_id=?1",
        "SELECT count(*) FROM inhouse_source_records WHERE form_id=?1",
        "SELECT count(*) FROM statics WHERE id=?1",
    ] {
        let count: u32 = db.query_row(sql, [id], |row| row.get(0)).unwrap();
        assert_eq!(count, 0, "unusable/deleted {id:08X} survived: {sql}");
    }
}

/// Bind a surviving native payload and canonical record to the expected supplier.
fn assert_winner(db: &Connection, id: u32, supplier: u32, payload: &[u8]) {
    let native: (u32, Vec<u8>) = db
        .query_row(
            "SELECT load_order,payload FROM inhouse_source_records WHERE form_id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(native, (supplier, payload.to_vec()), "winner {id:08X}");
    let canonical_supplier: u32 = db
        .query_row(
            "SELECT load_order FROM records WHERE form_id=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(canonical_supplier, supplier);
}

/// Rejected physical versions remain traceable without participating in winner selection.
fn assert_skipped(db: &Connection, id: u32, supplier: u32, reason_fragment: &str) {
    let (status, reason): (String, Option<String>) = db
        .query_row(
            "SELECT status,reason FROM record_overrides WHERE form_id=?1 AND plugin_id=?2",
            [id, supplier],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "skipped", "candidate {id:08X}");
    assert!(
        reason
            .as_deref()
            .is_some_and(|reason| reason.contains(reason_fragment)),
        "candidate {id:08X} reason {reason:?} lacks {reason_fragment}"
    );
}

/// Verify complete deletion, exact physical provenance, and independent typed body diagnostics.
fn assert_deletions(case: &DeletionCase, output: &Path) {
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    for deletion in &case.deletions {
        // This is the intended assertion-red on the integration commit.
        assert_absent(&db, deletion.runtime_id);
        let history: Vec<(u32, bool, String, Option<String>)> = db
            .prepare("SELECT plugin_id,deleted,status,reason FROM record_overrides WHERE form_id=?1 ORDER BY chain_position")
            .unwrap()
            .query_map([deletion.runtime_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let expected: Vec<(u32, bool, String, Option<String>)> = vec![
            (1, false, "accepted".to_owned(), None),
            (2, true, "accepted".to_owned(), None),
        ];
        assert_eq!(history, expected);
        let mut seen = 0;
        let summary = visit_source_history(
            output,
            None,
            SourceQuery::Physical {
                plugin_id: 2,
                source_record_offset: deletion.offset,
            },
            |version| {
                assert_eq!(version.runtime_form_id, Some(deletion.runtime_id));
                assert_eq!(version.source_form_id, deletion.source_id);
                assert_eq!(version.source_record_offset, deletion.offset);
                assert_eq!(
                    version.source_record_size as usize,
                    deletion.frame.len() - 24
                );
                assert_eq!(version.source_group_end, case.patch_original.len() as u64);
                assert!(version.frame_complete);
                assert_eq!(version.flags, 0x40020);
                assert_eq!(version.status, "accepted");
                assert!(version.rejection_reason.is_none());
                assert_eq!(version.identity.state, SourceIdentityState::Resolved);
                assert_eq!(
                    version.identity.full_local,
                    Some(deletion.source_id & 0x00ff_ffff)
                );
                assert_eq!(
                    version.identity.original_owner.as_deref(),
                    Some(if deletion.source_id >> 24 == 0 {
                        "owner.esm"
                    } else {
                        "delete.esp"
                    })
                );
                assert!(version.latest_source_candidate);
                assert!(version.latest_reader_admitted_candidate);
                assert!(!version.accepted_source_winner);
                assert!(!version.runtime_record_present);
                assert!(!version.runtime_fallback_before_latest_source);
                let raw = version.raw.unwrap();
                assert_eq!(raw.header.as_slice(), &deletion.frame[..24]);
                assert_eq!(raw.stored_payload.as_deref(), Some(&deletion.frame[24..]));
                assert_eq!(raw.access_status, "exact_native_header_and_stored_payload");
                seen += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(seen, 1);
        assert!(summary.fully_raw_available);
        assert!(!summary.typed_runtime_decoding_attempted);
        let typed = read_history(output, None, deletion.runtime_id).unwrap();
        assert_eq!(typed.versions.len(), 2);
        let last = &typed.versions[1];
        assert_eq!(last.status, "accepted");
        assert!(last.deleted);
        assert!(!last.winner);
        assert_eq!(
            last.decode_status,
            if deletion.typed_decodable {
                "decoded"
            } else {
                "undecodable"
            }
        );
        if !deletion.typed_decodable {
            assert!(
                last.decode_reason
                    .as_deref()
                    .is_some_and(|reason| !reason.is_empty())
            );
        }
    }
    assert_winner(&db, 0x0100_1820, 1, &case.retained_payload);
    assert_skipped(&db, 0x0100_1820, 2, "zlib");
    for (id, name) in [
        (0x0200_1841, "BeforeTombstones"),
        (0x0200_1842, "AfterTombstones"),
    ] {
        assert_winner(&db, id, 2, &static_payload(name));
    }
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    assert_eq!(diagnostics["decoder"]["delete.esp"]["skipped_records"], 1);
    assert_eq!(
        fs::read(case.fixture.data.join("Owner.esm")).unwrap(),
        case.owner_original
    );
    assert_eq!(
        fs::read(case.fixture.data.join("Delete.esp")).unwrap(),
        case.patch_original
    );
}

/// Empty and damaged compressed deletions remove objects while usable neighbors publish.
#[tokio::test]
async fn compressed_deletions_apply_through_complete_pipeline_publication() {
    let case = deletion_case();
    let output = case.fixture.publish().await;
    assert_deletions(&case, &output);
}

/// The full focused producer retains the same tombstone and exact-source contracts.
#[test]
fn compressed_deletions_apply_through_full_record_bundle() {
    let case = deletion_case();
    let output = case.fixture.bundle();
    assert_deletions(&case, &output);
}

/// Write authored heights with a constant native delta grid.
fn heights(offset: f32) -> Vec<u8> {
    let mut bytes = offset.to_le_bytes().to_vec();
    bytes.extend(vec![0; 33 * 33 + 3]);
    bytes
}

/// Native alpha anchors have distinct slot indices and preserved unused padding.
fn alpha(layer: u16) -> Vec<u8> {
    let mut bytes = 4u32.to_le_bytes().to_vec();
    bytes.extend([0, 0xA7]);
    bytes.extend(layer.to_le_bytes());
    bytes
}

/// Encode an authored vertex and opacity with unused-byte sentinels.
fn weight(vertex: u16, opacity: f32) -> Vec<u8> {
    let mut bytes = vertex.to_le_bytes().to_vec();
    bytes.extend([0xA7, 0x53]);
    bytes.extend(opacity.to_le_bytes());
    bytes
}

/// One complete LAND candidate gives layer A only its own vertex-two weights.
fn terrain(offset: Option<f32>, opacity: f32) -> Vec<u8> {
    let mut bytes = Vec::new();
    if let Some(offset) = offset {
        bytes.extend(subrecord(b"VHGT", &heights(offset)));
    }
    bytes.extend(subrecord(b"VCLR", &vec![111; 33 * 33 * 3]));
    bytes.extend(subrecord(b"ATXT", &alpha(1)));
    bytes.extend(subrecord(b"VTXT", &weight(2, opacity)));
    bytes
}

/// Place a new CELL and its LAND under the generated master's actual worldspace.
fn terrain_cell(cell: u32, land: &[u8]) -> Vec<u8> {
    let mut grid = (cell as i32).to_le_bytes().to_vec();
    grid.extend(30i32.to_le_bytes());
    grid.extend(0u32.to_le_bytes());
    let mut fields = subrecord(b"DATA", &[0]);
    fields.extend(subrecord(b"XCLC", &grid));
    let mut content = record(b"CELL", cell, 0, &fields);
    content.extend(group(6, cell, &group(9, cell, land)));
    group(1, 1, &content)
}

/// Distinct bad authored fields exercise NaN rejection and physical alpha-layer ownership.
#[derive(Clone, Copy)]
enum BadLayer {
    NanWeight,
    RejectedAnchor,
}

/// Retain expected native winners and the intended rejected signature for cache checks.
struct TerrainCase {
    fixture: Fixture,
    baseline: Vec<u8>,
    rejected_signature: &'static str,
    patch_original: Vec<u8>,
    heightless: Vec<u8>,
}

/// Create prior height 56, rejected override height 792, malformed first definition and neighbors.
fn terrain_case(bad: BadLayer) -> TerrainCase {
    let fixture = Fixture::new();
    let baseline = terrain(Some(7.0), 0.25);
    fixture.extend_base(&terrain_cell(0xB00, &record(b"LAND", 0xB01, 0, &baseline)));
    let (candidate, rejected_signature) = match bad {
        BadLayer::NanWeight => (terrain(Some(99.0), f32::NAN), "VTXT"),
        BadLayer::RejectedAnchor => {
            let mut bytes = terrain(Some(99.0), 0.25);
            bytes.extend(subrecord(b"ATXT", &alpha(3)[..7]));
            bytes.extend(subrecord(b"VTXT", &weight(9, 0.75)));
            (bytes, "ATXT")
        }
    };
    let mut patch = record(
        b"STAT",
        0x0100_0D01,
        0,
        &static_payload("BeforeBrokenTerrain"),
    );
    patch.extend(group(
        1,
        1,
        &group(
            6,
            0xB00,
            &group(9, 0xB00, &record(b"LAND", 0xB01, 0, &candidate)),
        ),
    ));
    // The same unusable authored data without a predecessor must not masquerade as heightless.
    patch.extend(terrain_cell(
        0x0100_0B20,
        &record(b"LAND", 0x0100_0B21, 0, &candidate),
    ));
    let heightless = terrain(None, 0.25);
    patch.extend(terrain_cell(
        0x0100_0B30,
        &record(b"LAND", 0x0100_0B31, 0, &heightless),
    ));
    if matches!(bad, BadLayer::NanWeight) {
        // Finite out-of-range values are independently rejected by existing terrain validation.
        fixture.extend_base(&terrain_cell(0xB10, &record(b"LAND", 0xB11, 0, &baseline)));
        patch.extend(group(
            1,
            1,
            &group(
                6,
                0xB10,
                &group(
                    9,
                    0xB10,
                    &record(b"LAND", 0xB11, 0, &terrain(Some(99.0), -0.25)),
                ),
            ),
        ));
    }
    patch.extend(record(
        b"STAT",
        0x0100_0D02,
        0,
        &static_payload("AfterBrokenTerrain"),
    ));
    let patch_original = fixture.plugin("BadNegative.esp", &["Skyrim.esm"], 0, &patch);
    fixture.order(&["Skyrim.esm", "BadNegative.esp"]);
    TerrainCase {
        fixture,
        baseline,
        rejected_signature,
        patch_original,
        heightless,
    }
}

/// Deserialize the actual cache artifact without normalizing away authored layer differences.
fn cache(output: &Path, name: &str) -> CellCache {
    rkyv::from_bytes::<CellCache, rkyv::rancor::Error>(&fs::read(output.join(name)).unwrap())
        .unwrap()
}

/// Both runtime and preserved caches must retain exactly the predecessor's alpha weights.
fn assert_layers(layers: &[TerrainLayer]) {
    assert_eq!(layers.len(), 2);
    assert!(layers[0].is_base);
    assert_eq!(layers[0].texture_form_id, 0);
    assert_eq!(
        (
            layers[1].texture_form_id,
            layers[1].quadrant,
            layers[1].layer,
            layers[1].is_base
        ),
        (4, 0, 1, false)
    );
    assert_eq!(
        layers[1].weights,
        [TerrainWeight {
            vertex: 2,
            opacity: 0.25
        }]
    );
}

/// Assert candidate-local recovery in native SQL, both caches, history and usable neighbors.
fn assert_terrain(case: &TerrainCase, output: &Path) {
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    // These are the intended assertion-red checks on the integration commit.
    for name in ["cell_cache.rkyv", "cell_cache_preserved.rkyv"] {
        let cache = cache(output, name);
        let prior = cache
            .cells
            .iter()
            .find(|cell| cell.cell_id == 0xB00)
            .unwrap();
        assert_eq!(
            prior.heights,
            vec![56.0; 33 * 33],
            "bad authored {} replaced the prior heights in {name}",
            case.rejected_signature
        );
        assert_eq!(prior.vertex_colors, vec![111; 33 * 33 * 3]);
        assert_layers(&prior.layers);
        assert!(!cache.cells.iter().any(|cell| cell.cell_id == 0x0100_0B20));
        let heightless = cache
            .cells
            .iter()
            .find(|cell| cell.cell_id == 0x0100_0B30)
            .unwrap();
        assert_eq!(heightless.heights, vec![0.0; 33 * 33]);
        assert_eq!(heightless.vertex_colors, vec![111; 33 * 33 * 3]);
        assert_layers(&heightless.layers);
        if case.rejected_signature == "VTXT" {
            let finite = cache
                .cells
                .iter()
                .find(|cell| cell.cell_id == 0xB10)
                .unwrap();
            assert_eq!(finite.heights, vec![56.0; 33 * 33]);
        }
    }
    assert_winner(&db, 0xB01, 0, &case.baseline);
    assert_skipped(&db, 0xB01, 1, case.rejected_signature);
    assert_absent(&db, 0x0100_0B21);
    assert_skipped(&db, 0x0100_0B21, 1, case.rejected_signature);
    assert_winner(&db, 0x0100_0B31, 1, &case.heightless);
    let heightmap: Vec<u8> = db
        .query_row(
            "SELECT heightmap FROM land WHERE cell_id=?1",
            [0xB00u32],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(heightmap, heights(7.0));
    let (supplier, layer_data): (u32, Vec<u8>) = db
        .query_row(
            "SELECT load_order,layer_data FROM inhouse_terrain_layers WHERE form_id=?1",
            [0xB01u32],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(supplier, 0);
    let layers = rkyv::from_bytes::<Vec<TerrainLayer>, rkyv::rancor::Error>(&layer_data).unwrap();
    assert_layers(&layers);
    if case.rejected_signature == "VTXT" {
        assert_winner(&db, 0xB11, 0, &case.baseline);
        assert_skipped(&db, 0xB11, 1, "VTXT");
    }
    for (id, name) in [
        (0x0100_0D01, "BeforeBrokenTerrain"),
        (0x0100_0D02, "AfterBrokenTerrain"),
    ] {
        assert_winner(&db, id, 1, &static_payload(name));
    }
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    assert_eq!(
        diagnostics["decoder"]["badnegative.esp"]["skipped_records"],
        if case.rejected_signature == "VTXT" {
            3
        } else {
            2
        }
    );
    assert_eq!(
        fs::read(case.fixture.data.join("BadNegative.esp")).unwrap(),
        case.patch_original
    );
}

/// NaN authored weights cannot replace height 56 with 792 in a complete published pack.
#[tokio::test]
async fn nan_land_override_keeps_prior_heights_through_complete_publication() {
    let case = terrain_case(BadLayer::NanWeight);
    let output = case.fixture.publish().await;
    assert_terrain(&case, &output);
}

/// Full records-only publication exercises the same NaN candidate rejection.
#[test]
fn nan_land_override_keeps_prior_heights_through_full_record_bundle() {
    let case = terrain_case(BadLayer::NanWeight);
    let output = case.fixture.bundle();
    assert_terrain(&case, &output);
}

/// A rejected seven-byte alpha anchor cannot give vertex-nine weights to layer A.
#[tokio::test]
async fn rejected_alpha_anchor_keeps_prior_layer_weights_through_complete_publication() {
    let case = terrain_case(BadLayer::RejectedAnchor);
    let output = case.fixture.publish().await;
    assert_terrain(&case, &output);
}

/// Full focused publication also rejects first definitions with unusable authored layers.
#[test]
fn rejected_alpha_anchor_keeps_prior_layer_weights_through_full_record_bundle() {
    let case = terrain_case(BadLayer::RejectedAnchor);
    let output = case.fixture.bundle();
    assert_terrain(&case, &output);
}
