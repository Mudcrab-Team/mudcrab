//! A source that changes after metadata admission does not reject usable neighbors.
use super::*;

/// Frame procedural native bytes with the independent 24-byte SE header layout.
fn frame(tag: &[u8; 4], id: u32, fields: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut payload = Vec::new();
    for (signature, bytes) in fields {
        payload.extend(signature);
        payload.extend(u16::try_from(bytes.len()).unwrap().to_le_bytes());
        payload.extend(bytes);
    }
    let mut bytes = tag.to_vec();
    bytes.extend(u32::try_from(payload.len()).unwrap().to_le_bytes());
    bytes.extend(0u32.to_le_bytes());
    bytes.extend(id.to_le_bytes());
    bytes.extend(0u32.to_le_bytes());
    bytes.extend(44u16.to_le_bytes());
    bytes.extend(0u16.to_le_bytes());
    bytes.extend(payload);
    bytes
}

/// Supply one valid header and keyword without using the converter's writer.
fn plugin(path: &Path, id: u32) {
    plugin_with_master(path, id, None);
}

/// A neighboring plugin may refer to the lost plugin's already-admitted master slot.
fn plugin_with_master(path: &Path, id: u32, master: Option<&str>) {
    let mut header = 1.7f32.to_le_bytes().to_vec();
    header.extend(1u32.to_le_bytes());
    header.extend(0x1000u32.to_le_bytes());
    let mut fields = vec![(*b"HEDR", header)];
    if let Some(master) = master {
        let mut name = master.as_bytes().to_vec();
        name.push(0);
        fields.push((*b"MAST", name));
        fields.push((*b"DATA", vec![0; 8]));
    }
    let mut bytes = frame(b"TES4", 0, &fields);
    bytes.extend(frame(
        b"KYWD",
        id,
        &[(*b"EDID", b"SnapshotNeighbor\0".to_vec())],
    ));
    std::fs::write(path, bytes).unwrap();
}

/// The changed source is renamed out of the selected path; no live source is truncated.
fn assert_missing_admitted_plugin_publishes(reader: crate::config::RecordReader) {
    let directory = tempfile::tempdir().unwrap();
    let paths =
        ["HealthyA.esm", "MutableMod.esp", "HealthyC.esp"].map(|name| directory.path().join(name));
    plugin(&paths[0], 0x811);
    plugin(&paths[1], 0x822);
    plugin_with_master(&paths[2], 0x0100_0933, Some("MutableMod.esp"));
    let unavailable = paths[1].clone();
    let retained = directory.path().join("removed-plugin.retained");
    // Reset even if publication panics, so another test on this thread cannot inherit the hook.
    struct ClearHook;
    impl Drop for ClearHook {
        fn drop(&mut self) {
            AFTER_PLUGIN_ADMISSION.with(|slot| *slot.borrow_mut() = None);
        }
    }
    let clear = ClearHook;
    AFTER_PLUGIN_ADMISSION.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(Box::new(move || {
            std::fs::rename(unavailable, retained).unwrap();
        }));
    });
    let database = directory.path().join("skyrim_world.db");
    let result = EsmParser::convert_plugins_with_reader_and_schemas(
        &paths,
        &database,
        reader,
        directory.path(),
        crate::config::DEFAULT_LANGUAGE,
        None,
    );
    drop(clear);
    assert!(!paths[1].exists(), "the after-admission hook must have run");
    assert!(
        result.is_ok(),
        "an unavailable admitted plugin must not abort publication: {result:?}"
    );
    let conversion = result.unwrap();
    assert_eq!(conversion.skipped_inputs, BTreeSet::from([1]));
    assert!(conversion.records.contains_key(&0x0000_0811));
    assert!(conversion.records.contains_key(&0x0200_0933));
    assert!(!conversion.records.contains_key(&0x0100_0822));
    let warnings: Vec<_> = conversion
        .warnings
        .iter()
        .filter(|warning| warning.to_lowercase().contains("mutablemod.esp"))
        .collect();
    assert_eq!(
        warnings.len(),
        1,
        "the unavailable input must warn exactly once: {warnings:?}"
    );
    let conn = Connection::open(database).unwrap();
    let rows = conn.prepare("SELECT id,name,priority,length(checksum),typeof(checksum) FROM plugins ORDER BY priority")
        .unwrap().query_map([], |row| Ok((row.get::<_,i64>(0)?, row.get::<_,String>(1)?,
            row.get::<_,i64>(2)?, row.get::<_,i64>(3)?, row.get::<_,String>(4)?)))
        .unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
    assert_eq!(rows.len(), 3);
    for (index, (id, name, priority, checksum_len, checksum_type)) in rows.iter().enumerate() {
        assert_eq!((*id, *priority), (index as i64, index as i64));
        assert_eq!(name, paths[index].file_name().unwrap().to_str().unwrap());
        assert_eq!(*checksum_len, if index == 1 { 0 } else { 32 });
        assert_eq!(checksum_type, "blob");
    }
    let (owner, local): (String, u32) = conn
        .query_row(
            "SELECT plugin_name,internal_id FROM formid_map WHERE form_id=?1",
            [0x0200_0933u32],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(owner.eq_ignore_ascii_case("HealthyC.esp"));
    assert_eq!(local, 0x933);
    if reader == crate::config::RecordReader::Inhouse {
        // Fidelity inspects retained SQL, so a missing source cannot abort the audit.
        let schema =
            crate::records::schema_format::Schema::parse(crate::records::SCHEMA_BYTES).unwrap();
        crate::records::fidelity::audit_storage(&conn, &schema).unwrap();
    }
}

#[test]
fn missing_admitted_plugin_legacy_publishes_neighbors_and_keeps_slots() {
    assert_missing_admitted_plugin_publishes(crate::config::RecordReader::Legacy);
}

#[test]
fn missing_admitted_plugin_inhouse_publishes_neighbors_and_keeps_slots() {
    assert_missing_admitted_plugin_publishes(crate::config::RecordReader::Inhouse);
}

/// A late unusable input preserves both surrounding plugins and their admitted slots.
#[test]
fn legacy_snapshot_input_failure_keeps_neighbor_plugins() {
    let directory = tempfile::tempdir().unwrap();
    let paths =
        ["HealthyA.esm", "MutableMod.esp", "HealthyC.esp"].map(|name| directory.path().join(name));
    for (path, id) in paths.iter().zip([0x811, 0x822, 0x933]) {
        plugin(path, id);
    }
    let order = load_order::LoadOrder::read(&paths).unwrap();
    // No live map exists here: truncation between admission and reading is safe on both OSes.
    std::fs::write(&paths[1], b"TES4").unwrap();
    let result = EsmParser::merge_plugins_with_load_order(&paths, &order);
    let conversion = match result {
        Ok(merge) => merge.conversion,
        Err(error) => {
            panic!("unusable plugin input must not abort neighboring plugins: {error:#}")
        }
    };
    assert!(conversion.records.contains_key(&0x0000_0811));
    assert!(conversion.records.contains_key(&0x0200_0933));
    assert!(!conversion.records.contains_key(&0x0100_0822));
    assert_eq!(conversion.warnings.len(), 1);
    assert!(conversion.warnings[0].contains("mutablemod.esp"));
    assert!(conversion.warnings[0].contains("skipped unusable plugin input"));
}

/// Header facts that change the interpretation of an already assigned plugin slot.
#[derive(Clone, Copy, Debug)]
enum HeaderReplacement {
    Masters,
    Esm,
    Light,
    Localized,
}

/// Reset the deterministic replacement hook even if a regression assertion fails.
struct HeaderReplacementHook;

impl Drop for HeaderReplacementHook {
    fn drop(&mut self) {
        AFTER_PLUGIN_ADMISSION.with(|slot| *slot.borrow_mut() = None);
    }
}

/// Replace the pathname after admission, without truncating any mapped source.
fn install_header_replacement(
    changed: &Path,
    replacement: &Path,
    retained: &Path,
    master: &str,
    kind: HeaderReplacement,
) -> HeaderReplacementHook {
    write_header_replacement(replacement, master, kind);
    let (changed, replacement, retained) = (
        changed.to_path_buf(),
        replacement.to_path_buf(),
        retained.to_path_buf(),
    );
    AFTER_PLUGIN_ADMISSION.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(Box::new(move || {
            std::fs::rename(changed.clone(), retained).unwrap();
            std::fs::rename(replacement, changed).unwrap();
        }));
    });
    HeaderReplacementHook
}

/// Build the replacement generation independently for cold and seeded-cache controls.
fn write_header_replacement(replacement: &Path, master: &str, kind: HeaderReplacement) {
    plugin_with_master(
        replacement,
        0x822,
        matches!(kind, HeaderReplacement::Masters).then_some(master),
    );
    let mut bytes = std::fs::read(replacement).unwrap();
    let flags = match kind {
        HeaderReplacement::Masters => 0u32,
        HeaderReplacement::Esm => 1,
        HeaderReplacement::Light => 0x200,
        HeaderReplacement::Localized => 0x80,
    };
    bytes[8..12].copy_from_slice(&flags.to_le_bytes());
    std::fs::write(replacement, bytes).unwrap();
}

/// A new header cannot reinterpret a middle plugin under the old slot/master authority.
fn assert_changed_admitted_header(reader: crate::config::RecordReader, kind: HeaderReplacement) {
    let directory = tempfile::tempdir().unwrap();
    let paths =
        ["HealthyA.esm", "MutableMod.esp", "HealthyC.esp"].map(|name| directory.path().join(name));
    plugin(&paths[0], 0x811);
    plugin(&paths[1], 0x822);
    plugin_with_master(&paths[2], 0x0100_0933, Some("MutableMod.esp"));
    let retained = directory.path().join("original-middle.retained");
    let hook = install_header_replacement(
        &paths[1],
        &directory.path().join("replacement.native"),
        &retained,
        "HealthyA.esm",
        kind,
    );
    let database = directory.path().join("world.db");
    let result = EsmParser::convert_plugins_with_reader_and_schemas(
        &paths,
        &database,
        reader,
        directory.path(),
        crate::config::DEFAULT_LANGUAGE,
        None,
    );
    drop(hook);
    assert!(
        retained.is_file(),
        "replacement must happen after admission"
    );
    assert!(
        result.is_ok(),
        "header replacement must not abort neighbors: {result:?}"
    );
    let result = result.unwrap();
    assert!(result.records.contains_key(&0x811));
    assert!(result.records.contains_key(&0x0200_0933));
    assert!(
        result.skipped_inputs.contains(&1),
        "changed admitted header must be skipped before decoding records: {reader:?} {kind:?}"
    );
    assert!(!result.records.contains_key(&0x0100_0822));
    assert!(!result.records.contains_key(&0x0000_0822));
    let warnings = result
        .warnings
        .iter()
        .filter(|warning| warning.to_ascii_lowercase().contains("mutablemod.esp"))
        .collect::<Vec<_>>();
    assert_eq!(warnings.len(), 1, "one bounded input warning: {warnings:?}");
    assert!(
        warnings[0].contains("changed after load order was read"),
        "{warnings:?}"
    );
    let connection = Connection::open(database).unwrap();
    let slots = connection
        .prepare("SELECT id,priority,length(checksum) FROM plugins ORDER BY id")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, u32>(2)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(slots, [(0, 0, 32), (1, 1, 0), (2, 2, 32)]);
    let (owner, local): (String, u32) = connection
        .query_row(
            "SELECT plugin_name,internal_id FROM formid_map WHERE form_id=?1",
            [0x0200_0933u32],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(owner.eq_ignore_ascii_case("HealthyC.esp"));
    assert_eq!(local, 0x933);
}

/// Changed ordered masters are rejected by the legacy reader before remapping.
#[test]
fn changed_admitted_masters_legacy_keep_neighbor_slots() {
    assert_changed_admitted_header(
        crate::config::RecordReader::Legacy,
        HeaderReplacement::Masters,
    );
}

/// Changed ordered masters are rejected by the in-house reader before remapping.
#[test]
fn changed_admitted_masters_inhouse_keep_neighbor_slots() {
    assert_changed_admitted_header(
        crate::config::RecordReader::Inhouse,
        HeaderReplacement::Masters,
    );
}

/// Changed ESM flags cannot silently reuse legacy admission facts.
#[test]
fn changed_admitted_esm_legacy_keep_neighbor_slots() {
    assert_changed_admitted_header(crate::config::RecordReader::Legacy, HeaderReplacement::Esm);
}

/// Changed ESM flags cannot silently reuse in-house admission facts.
#[test]
fn changed_admitted_esm_inhouse_keep_neighbor_slots() {
    assert_changed_admitted_header(crate::config::RecordReader::Inhouse, HeaderReplacement::Esm);
}

/// A newly light plugin must not be decoded in the old full slot by the legacy reader.
#[test]
fn changed_admitted_light_legacy_keep_neighbor_slots() {
    assert_changed_admitted_header(
        crate::config::RecordReader::Legacy,
        HeaderReplacement::Light,
    );
}

/// A newly light plugin must not be decoded in the old full slot by the in-house reader.
#[test]
fn changed_admitted_light_inhouse_keep_neighbor_slots() {
    assert_changed_admitted_header(
        crate::config::RecordReader::Inhouse,
        HeaderReplacement::Light,
    );
}

/// No-LOD publication must not seal a cache for bytes interpreted with stale header facts.
async fn assert_changed_header_has_no_database_cache(
    reader: crate::config::RecordReader,
    seed_cache: bool,
) {
    for kind in [
        HeaderReplacement::Masters,
        HeaderReplacement::Esm,
        HeaderReplacement::Light,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        std::fs::create_dir(&data).unwrap();
        dummy_content::layout::generate(
            &data,
            dummy_content::layout::DEFAULT_SEED,
            dummy_content::layout::Formats::parse("dds,nif").unwrap(),
        )
        .unwrap();
        std::fs::write(
            data.join("Skyrim.esm"),
            dummy_content::esm::plugin(&dummy_content::esm::Plugin {
                author: "ChangedHeaderFixture",
                worldspace: "ChangedHeaderWorld",
                cells: &[dummy_content::esm::Cell {
                    grid_x: 0,
                    grid_y: 0,
                }],
                model_path: dummy_content::layout::GENERATED_MODEL_PATH,
                diffuse: dummy_content::layout::GENERATED_DIFFUSE_PATH,
                normal_texture: dummy_content::layout::GENERATED_NORMAL_PATH,
            })
            .unwrap(),
        )
        .unwrap();
        plugin(&data.join("MutableMod.esp"), 0x822);
        plugin_with_master(
            &data.join("HealthyC.esp"),
            0x0100_0933,
            Some("MutableMod.esp"),
        );
        let selection = directory.path().join("plugins.txt");
        std::fs::write(&selection, b"*MutableMod.esp\n*HealthyC.esp\n").unwrap();
        let retained = directory.path().join("original-middle.retained");
        let output = directory.path().join("modern");
        let mut config = crate::PipelineConfig::new(&data, &output);
        config.plugins_file = Some(selection);
        config.record_reader = reader;
        config.no_lod = true;
        config.cpu_jobs = 2;
        config.terrain_default_texture = crate::config::TerrainDefaultTexture::White;
        let cache_root = config.ingestion_cache_dir().join(".database-cache");
        let original = std::fs::read(data.join("MutableMod.esp")).unwrap();
        if seed_cache {
            write_header_replacement(&data.join("MutableMod.esp"), "Skyrim.esm", kind);
            let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
            let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
            let seeded = crate::AssetPipeline::run_async(config.clone(), sender)
                .await
                .unwrap();
            drain.await.unwrap();
            assert!(
                seeded.database_cache_key.is_some(),
                "the replacement generation must seed cache proof"
            );
            assert!(!seeded.database_cache_hit);
            std::fs::write(data.join("MutableMod.esp"), &original).unwrap();
        }
        let cache_entries_before = std::fs::read_dir(&cache_root)
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .filter(|entry| entry.path().is_dir())
                    .count()
            })
            .unwrap_or(0);
        assert_eq!(cache_entries_before > 0, seed_cache);
        // Admission now sees A; the post-admission hook restores generation B, whose cache
        // identity is already valid in the seeded control, before either cache check runs.
        let hook = install_header_replacement(
            &data.join("MutableMod.esp"),
            &directory.path().join("replacement.native"),
            &retained,
            "Skyrim.esm",
            kind,
        );
        let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
        let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
        let result = crate::AssetPipeline::run_async(config, sender).await;
        drain.await.unwrap();
        drop(hook);
        assert!(
            retained.is_file(),
            "replacement must happen after admission"
        );
        assert!(
            result.is_ok(),
            "changed header must publish neighbors: {result:?}"
        );
        let report = result.unwrap();
        let connection = Connection::open(output.join("skyrim_world.db")).unwrap();
        let neighbor: u32 = connection
            .query_row(
                "SELECT count(*) FROM records WHERE form_id=?1",
                [0x0200_0933u32],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(neighbor, 1);
        assert!(
            report.database_cache_key.is_none(),
            "changed admitted headers must not receive a reusable database cache key: {reader:?} {kind:?}"
        );
        assert!(!report.database_cache_hit);
        assert!(!report.complete);
        let warnings = report
            .warnings
            .iter()
            .filter(|warning| warning.to_ascii_lowercase().contains("mutablemod.esp"))
            .collect::<Vec<_>>();
        assert_eq!(
            warnings.len(),
            1,
            "one bounded header mismatch warning: {warnings:?}"
        );
        assert!(
            warnings[0].contains("changed after load order was read"),
            "{warnings:?}"
        );
        let checksum: u32 = connection
            .query_row(
                "SELECT length(checksum) FROM plugins WHERE id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(checksum, 0);
        let cached = std::fs::read_dir(&cache_root)
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .filter(|entry| entry.path().is_dir())
                    .count()
            })
            .unwrap_or(0);
        assert_eq!(
            cached, cache_entries_before,
            "mismatched header must never add a database cache entry"
        );
    }
}

/// Cache identities created after extraction do not excuse stale legacy admission.
#[tokio::test(flavor = "current_thread")]
async fn changed_admitted_header_no_lod_legacy_never_caches() {
    assert_changed_header_has_no_database_cache(crate::config::RecordReader::Legacy, false).await;
}

/// Cache identities created after extraction do not excuse stale in-house admission.
#[tokio::test(flavor = "current_thread")]
async fn changed_admitted_header_no_lod_inhouse_never_caches() {
    assert_changed_header_has_no_database_cache(crate::config::RecordReader::Inhouse, false).await;
}

/// An existing replacement-generation cache cannot bypass the legacy admission check.
#[tokio::test(flavor = "current_thread")]
async fn changed_admitted_header_legacy_rejects_seeded_cache() {
    assert_changed_header_has_no_database_cache(crate::config::RecordReader::Legacy, true).await;
}

/// An existing replacement-generation cache cannot bypass the in-house admission check.
#[tokio::test(flavor = "current_thread")]
async fn changed_admitted_header_inhouse_rejects_seeded_cache() {
    assert_changed_header_has_no_database_cache(crate::config::RecordReader::Inhouse, true).await;
}

/// Localized-field interpretation must use the same flag facts as admission in legacy mode.
#[test]
fn changed_admitted_localized_legacy_keep_neighbor_slots() {
    assert_changed_admitted_header(
        crate::config::RecordReader::Legacy,
        HeaderReplacement::Localized,
    );
}

/// Localized-field interpretation must use the same flag facts as admission in in-house mode.
#[test]
fn changed_admitted_localized_inhouse_keep_neighbor_slots() {
    assert_changed_admitted_header(
        crate::config::RecordReader::Inhouse,
        HeaderReplacement::Localized,
    );
}

/// A record set keeps the digest of the bytes it was decoded from, not a later replacement.
fn assert_decoded_checksum_survives_body_edit(reader: crate::config::RecordReader) {
    let directory = tempfile::tempdir().unwrap();
    let paths = [
        directory.path().join("HealthyA.esm"),
        directory.path().join("MutableMod.esp"),
        directory.path().join("HealthyC.esp"),
    ];
    plugin(&paths[0], 0x811);
    plugin_with_master(&paths[1], 0x822, Some("HealthyA.esm"));
    plugin_with_master(&paths[2], 0x0100_0933, Some("MutableMod.esp"));
    let decoded_bytes = std::fs::read(&paths[1]).unwrap();
    let decoded_digest = crate::cache::hash_bytes(&decoded_bytes);
    // The replacement keeps the TES4 flags and ordered masters but changes the body.
    let replacement = directory.path().join("replacement.native");
    plugin_with_master(&replacement, 0x833, Some("HealthyA.esm"));
    let replacement_digest = crate::cache::hash_bytes(&std::fs::read(&replacement).unwrap());
    assert_ne!(decoded_digest, replacement_digest);
    let changed = paths[1].clone();
    let retained = directory.path().join("original-middle.retained");
    struct ClearHook;
    impl Drop for ClearHook {
        fn drop(&mut self) {
            AFTER_PLUGIN_DECODE.with(|slot| *slot.borrow_mut() = None);
        }
    }
    let clear = ClearHook;
    AFTER_PLUGIN_DECODE.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(Box::new(move || {
            std::fs::rename(&changed, retained).unwrap();
            std::fs::rename(replacement, changed).unwrap();
        }));
    });
    let database = directory.path().join("world.db");
    let result = EsmParser::convert_plugins_with_reader_and_schemas(
        &paths,
        &database,
        reader,
        directory.path(),
        crate::config::DEFAULT_LANGUAGE,
        None,
    );
    drop(clear);
    assert!(
        result.is_ok(),
        "a body-only replacement must not abort neighbors: {result:?}"
    );
    let conversion = result.unwrap();
    assert!(conversion.records.contains_key(&0x0000_0822));
    assert!(!conversion.records.contains_key(&0x0000_0833));
    let connection = Connection::open(database).unwrap();
    let stored: String = connection
        .query_row(
            "SELECT lower(hex(checksum)) FROM plugins WHERE priority=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        stored, decoded_digest,
        "the stored checksum must cover the bytes the records were decoded from"
    );
    assert_ne!(
        stored, replacement_digest,
        "a later replacement generation must never supply the decoded records' checksum: {reader:?}"
    );
}

/// The legacy reader binds the stored checksum to its decoded view.
#[test]
fn decoded_checksum_legacy_ignores_body_edit_after_decode() {
    assert_decoded_checksum_survives_body_edit(crate::config::RecordReader::Legacy);
}

/// The in-house reader binds the stored checksum to its decoded view.
#[test]
fn decoded_checksum_inhouse_ignores_body_edit_after_decode() {
    assert_decoded_checksum_survives_body_edit(crate::config::RecordReader::Inhouse);
}
