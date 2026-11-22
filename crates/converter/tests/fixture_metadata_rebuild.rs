mod common;

use converter::{
    AssetPipeline, PipelineConfig,
    cache::{ConversionManifest, hash_bytes, hash_file},
};
use dummy_content::{Entry, bsa, esm, layout, nif};
use rusqlite::Connection;
use std::{fs, path::Path};

const PACKED_SETTINGS: [u8; 16] = [0xfc, 0xff, 0xfc, 0xff, 32, 0, 0, 0, 4, 0, 0, 0, 32, 0, 0, 0];
const VOLCANIC_MODEL: &str = "meshes/landscape/grass/dlc02volcanicashgrass01.nif";
const VOLCANIC_GLB: &str = "meshes/landscape/grass/dlc02volcanicashgrass01.glb";
const VOLCANIC_ORIGINAL: &str = "textures/landscape/grass/volcanicashgrass02_n.dds";
const VOLCANIC_TARGET: &str = "textures/dlc02/landscape/grass/volcanicashgrass02_n.dds";

/// Reframe the generated plugin with the same valid base in all four quadrants.
fn paint_every_quadrant(bytes: &[u8]) -> Vec<u8> {
    use dummy_content::inhouse_magic::subrecord;
    let mut output = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let header = &bytes[offset..offset + 24];
        let size = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
        let group = &header[..4] == b"GRUP";
        let end = offset + if group { size } else { size + 24 };
        let payload = &bytes[offset + 24..end];
        let payload = if group {
            paint_every_quadrant(payload)
        } else if &header[..4] == b"LAND" {
            let mut payload = payload.to_vec();
            // The generated LAND ends in BTXT, naming LTEX 4 in quadrant zero.
            assert_eq!(&payload[payload.len() - 14..payload.len() - 10], b"BTXT");
            for quadrant in 1..4 {
                payload.extend(subrecord(b"BTXT", &[4, 0, 0, 0, quadrant, 0, 0, 0]));
            }
            payload
        } else {
            payload.to_vec()
        };
        let mut header = header.to_vec();
        header[4..8].copy_from_slice(
            &u32::try_from(payload.len() + if group { 24 } else { 0 })
                .unwrap()
                .to_le_bytes(),
        );
        output.extend(header);
        output.extend(payload);
        offset = end;
    }
    output
}

/// Pin each reader's established dangling-base projection through real publication.
async fn dangling_base_publication(reader: converter::config::RecordReader, expected: u32) {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("published");
    generate(&data);
    let plugin = data.join("Skyrim.esm");
    let mut bytes = paint_every_quadrant(&fs::read(&plugin).unwrap());
    let base = bytes.windows(4).position(|tag| tag == b"BTXT").unwrap();
    assert_eq!(&bytes[base + 4..base + 6], &8u16.to_le_bytes());
    bytes[base + 6..base + 10].copy_from_slice(&0x899u32.to_le_bytes());
    fs::write(plugin, bytes).unwrap();
    let mut config = common::cpu_lod_config(&data, &output);
    config.record_reader = reader;
    config.no_lod = true;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    let mapping =
        converter::esm::cell_cache::validate_cell_cache(&output.join("cell_cache.rkyv")).unwrap();
    let cache =
        rkyv::from_bytes::<converter::shared::CellCache, rkyv::rancor::Error>(&mapping).unwrap();
    let layers = &cache.cells[0].layers;
    assert_eq!(layers.len(), 4);
    assert_eq!(
        layers
            .iter()
            .find(|layer| layer.quadrant == 0 && layer.is_base)
            .unwrap()
            .texture_form_id,
        expected
    );
    assert_eq!(
        layers
            .iter()
            .find(|layer| layer.quadrant == 1 && layer.is_base)
            .unwrap()
            .texture_form_id,
        4
    );
    let database = Connection::open(output.join("skyrim_world.db")).unwrap();
    let diffuse: Option<String> = database
        .query_row(
            "SELECT diffuse_path FROM terrain_default_texture WHERE id=0",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        diffuse.as_deref(),
        (expected == 0).then_some("terrain/default-diffuse.ktx2")
    );
    let neighbor: u32 = database
        .query_row(
            "SELECT COUNT(*) FROM \"references\" WHERE id=4660",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(neighbor, 1);
    if reader == converter::config::RecordReader::Inhouse {
        assert!(
            report
                .notices
                .iter()
                .any(|notice| notice.contains("cleared 1 links") && notice.contains("00000899")),
            "the existing optional-link recovery must be reported: {report:?}"
        );
        assert!(report.complete && report.warnings.is_empty(), "{report:?}");
    } else {
        assert!(report.complete && report.warnings.is_empty(), "{report:?}");
        assert!(
            !report
                .notices
                .iter()
                .any(|notice| notice.contains("00000899")),
            "legacy preserves unresolved identity without an optional-link recovery notice: {report:?}"
        );
    }
}

/// The in-house reader clears a dangling BTXT to null, which selects the configured default.
#[tokio::test]
async fn inhouse_dangling_base_uses_configured_default() {
    dangling_base_publication(converter::config::RecordReader::Inhouse, 0).await;
}

/// Legacy keeps a dangling nonzero BTXT, so it does not become a configured null base.
#[tokio::test]
async fn legacy_dangling_base_retains_unresolved_identity() {
    dangling_base_publication(converter::config::RecordReader::Legacy, 0x899).await;
}

/// A used default that cannot be read, converted or resolved fails the rebuild (#811, SI-23).
/// No generated white texture replaces it, and the source pack stays unchanged.
fn assert_default_failure(
    result: &Result<converter::PipelineReport, impl std::fmt::Debug>,
    what: &str,
) {
    let message = format!("{result:?}");
    assert!(
        result.is_err(),
        "{what}: expected a failed rebuild, got {message}"
    );
    assert!(
        message.contains("no white texture replaces it"),
        "{what}: {message}"
    );
}

/// Verify complete publication, white default metadata, and an intact neighboring record.
fn assert_white_rebuild(report: &converter::PipelineReport, output: &Path, needed: bool) {
    assert!(report.complete, "{report:?}");
    assert!(report.warnings.is_empty(), "{report:?}");
    assert!(
        ConversionManifest::load(&output.join("conversion-manifest.json"))
            .unwrap()
            .complete
    );
    let database = Connection::open(output.join("skyrim_world.db")).unwrap();
    let default: (Option<String>, Option<String>, Option<String>) = database
        .query_row("SELECT diffuse_path, normal_path, source_diffuse FROM terrain_default_texture WHERE id=0", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap();
    assert_eq!(default, (None, None, None));
    let neighbors: u32 = database
        .query_row(
            "SELECT COUNT(*) FROM \"references\" WHERE id=4660",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(neighbors, 1);
    let mapping =
        converter::esm::cell_cache::validate_cell_cache(&output.join("cell_cache.rkyv")).unwrap();
    let cache =
        rkyv::from_bytes::<converter::shared::CellCache, rkyv::rancor::Error>(&mapping).unwrap();
    assert_eq!(cache.cells.len(), 1);
    assert_eq!(cache.cells[0].layers.len(), if needed { 1 } else { 4 });
    let bases: Vec<_> = cache.cells[0]
        .layers
        .iter()
        .map(|layer| (layer.quadrant, layer.texture_form_id))
        .collect();
    assert_eq!(
        bases,
        if needed {
            vec![(0, 4)]
        } else {
            vec![(0, 4), (1, 4), (2, 4), (3, 4)]
        }
    );
    if !needed {
        assert!(
            !report
                .notices
                .iter()
                .any(|notice| notice.contains("default land")),
            "{:?}",
            report.notices
        );
    }
}

/// Exercise a genuinely unreadable loose default through full metadata publication.
#[cfg(any(windows, unix))]
async fn rebuild_with_unreadable_default(needed: bool) {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("rebuilt");
    generate(&data);
    if !needed {
        let plugin = data.join("Skyrim.esm");
        fs::write(&plugin, paint_every_quadrant(&fs::read(&plugin).unwrap())).unwrap();
    }
    convert(&data, &source).await;
    let manifest_hash = hash_file(&source.join("conversion-manifest.json")).unwrap();
    let default = data.join("textures/landscape/dirt02.dds");
    // A failed winning loose override must not expose a healthy lower-priority archive.
    fs::write(
        data.join("Skyrim - Textures.bsa"),
        bsa::v105(
            &[Entry {
                name: "textures/landscape/dirt02.dds",
                data: &fs::read(&default).unwrap(),
            }],
            bsa::Compression::None,
        )
        .unwrap(),
    )
    .unwrap();
    let Some(unreadable) = block_file_reads(&default) else {
        return;
    };
    assert!(
        fs::read(&default).is_err(),
        "fixture must actually deny reads"
    );
    let result = rebuild(&data, &source, &output).await;
    drop(unreadable);
    if needed {
        assert_default_failure(&result, "unreadable loose default");
    } else {
        assert_white_rebuild(
            result.as_ref().expect("an unused default is never opened"),
            &output,
            false,
        );
    }
    assert_eq!(
        hash_file(&source.join("conversion-manifest.json")).unwrap(),
        manifest_hash
    );
}

/// An unused default must never be opened when every quadrant has an authored base.
#[cfg(any(windows, unix))]
#[tokio::test]
async fn metadata_rebuild_default_unused_unreadable_loose_completes() {
    rebuild_with_unreadable_default(false).await;
}

/// An unreadable used default fails the rebuild; no white texture replaces it (#811).
#[cfg(any(windows, unix))]
#[tokio::test]
async fn metadata_rebuild_default_needed_unreadable_loose_fails() {
    rebuild_with_unreadable_default(true).await;
}

/// A damaged archive-only default fails the rebuild; no white texture replaces it (#811).
#[tokio::test]
async fn metadata_rebuild_default_changed_archive_fails() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("rebuilt");
    generate(&data);
    convert(&data, &source).await;
    let default = data.join("textures/landscape/dirt02.dds");
    fs::remove_file(default).unwrap();
    fs::write(
        data.join("Skyrim - Textures.bsa"),
        bsa::v105(
            &[Entry {
                name: "textures/landscape/dirt02.dds",
                data: b"damaged replacement DDS",
            }],
            bsa::Compression::None,
        )
        .unwrap(),
    )
    .unwrap();
    let result = rebuild(&data, &source, &output).await;
    assert_default_failure(&result, "damaged archive default");
}

/// Regenerated default images use current loose or archive bytes, unlike retained LTEX assets.
#[tokio::test]
async fn metadata_rebuild_defaults_use_current_bytes() {
    use converter::texture::{TextureConverter, TextureEncoding};
    use dummy_content::{dds, rng::Rng};
    for (archived, change_diffuse) in [(false, false), (false, true), (true, false), (true, true)] {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let source = directory.path().join("source");
        let output = directory.path().join("rebuilt");
        generate(&data);
        convert(&data, &source).await;
        let manifest_hash = hash_file(&source.join("conversion-manifest.json")).unwrap();
        let mut expected = Vec::new();
        let mut replacements = Vec::new();
        for (name, format, encoding, seed) in [
            (
                "dirt02",
                dds::Format::Bc1Unorm,
                TextureEncoding::ColorSrgb,
                281,
            ),
            (
                "dirt02_n",
                dds::Format::Bc5Unorm,
                TextureEncoding::NormalLinear,
                307,
            ),
        ] {
            let relative = format!("textures/landscape/{name}.dds");
            let original = fs::read(data.join(&relative)).unwrap();
            let bytes = if name == "dirt02" && !change_diffuse {
                original.clone()
            } else {
                let bytes =
                    dds::generate(&dds::Spec::new(format, 16, 16), &mut Rng::new(seed)).unwrap();
                assert_ne!(bytes, original);
                bytes
            };
            let config = common::cpu_lod_config(&data, &output);
            expected.push(
                TextureConverter::convert_with_options(
                    &bytes,
                    encoding,
                    config.texture_fallback_quality,
                    config.texture_uastc_level,
                    config.texture_zstd_level,
                )
                .unwrap(),
            );
            if archived {
                fs::remove_file(data.join(&relative)).unwrap();
            } else {
                fs::write(data.join(&relative), &bytes).unwrap();
            }
            replacements.push((relative, bytes));
        }
        if archived {
            let entries: Vec<_> = replacements
                .iter()
                .map(|(name, data)| Entry { name, data })
                .collect();
            fs::write(
                data.join("Skyrim - Textures.bsa"),
                bsa::v105(&entries, bsa::Compression::None).unwrap(),
            )
            .unwrap();
        }
        let report = rebuild(&data, &source, &output).await.unwrap();
        assert!(report.complete, "{report:?}");
        assert!(report.warnings.is_empty(), "{report:?}");
        for (name, bytes) in ["diffuse", "normal"].into_iter().zip(expected) {
            assert!(
                output
                    .join(format!("terrain/default-{name}.ktx2"))
                    .is_file(),
                "regenerated default {name} must use the current source (archived={archived})"
            );
            assert_eq!(
                fs::read(output.join(format!("terrain/default-{name}.ktx2"))).unwrap(),
                bytes
            );
        }
        let database = Connection::open(output.join("skyrim_world.db")).unwrap();
        let normal: String = database
            .query_row(
                "SELECT normal_path FROM terrain_default_texture WHERE id=0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(normal, "terrain/default-normal.ktx2");
        let neighbor: u32 = database
            .query_row(
                "SELECT COUNT(*) FROM \"references\" WHERE id=4660",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(neighbor, 1);
        assert_eq!(
            hash_file(&source.join("conversion-manifest.json")).unwrap(),
            manifest_hash
        );
    }
}

/// A corrupt compressed default entry fails the rebuild; no white texture replaces it (#811).
#[tokio::test]
async fn metadata_rebuild_default_damaged_compressed_archive_fails() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("rebuilt");
    generate(&data);
    convert(&data, &source).await;
    let default = data.join("textures/landscape/dirt02.dds");
    let dds = fs::read(&default).unwrap();
    let mut archive = bsa::v105(
        &[Entry {
            name: "textures/landscape/dirt02.dds",
            data: &dds,
        }],
        bsa::Compression::Lz4,
    )
    .unwrap();
    // One SSE folder record (24 bytes), then its length-prefixed NUL-terminated name.
    let file_record = 36 + 24 + 1 + "textures/landscape".len() + 1;
    let payload = u32::from_le_bytes(
        archive[file_record + 12..file_record + 16]
            .try_into()
            .unwrap(),
    ) as usize;
    archive[payload + 4..].fill(0xff);
    fs::write(data.join("Skyrim - Textures.bsa"), archive).unwrap();
    fs::remove_file(default).unwrap();
    let result = rebuild(&data, &source, &output).await;
    assert_default_failure(&result, "damaged compressed default");
}

/// Default recovery must not relax source identity checks for an LTEX-required diffuse.
#[tokio::test]
async fn metadata_rebuild_default_recovery_keeps_ordinary_sources_strict() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("rebuilt");
    generate(&data);
    convert(&data, &source).await;
    fs::write(
        data.join(layout::GENERATED_DIFFUSE_PATH),
        b"changed ordinary terrain DDS",
    )
    .unwrap();
    let error = rebuild(&data, &source, &output).await.unwrap_err();
    assert!(
        format!("{error:#}").contains("retained terrain texture source differs"),
        "{error:#}"
    );
    assert!(!output.exists());
}

#[tokio::test]
async fn metadata_rebuild_matches_terrain_archive_precedence_used_by_conversion() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("rebuilt");
    generate(&data);
    let diffuse = layout::GENERATED_DIFFUSE_PATH;
    let dds = fs::read(data.join(diffuse)).unwrap();
    fs::remove_file(data.join(diffuse)).unwrap();
    let archive = |name, member, bytes| {
        fs::write(
            data.join(name),
            bsa::v105(&[Entry::new(member, bytes)], bsa::Compression::None).unwrap(),
        )
        .unwrap();
    };
    archive("Skyrim - A.bsa", diffuse, b"earlier runtime DDS");
    archive("Skyrim - B.bsa", diffuse, &dds);
    archive(
        "Skyrim - C.bsa",
        "CalienteTools/Textures/Generated_Color.DDS",
        b"later authoring DDS",
    );
    convert(&data, &source).await;

    let report = rebuild(&data, &source, &output).await.unwrap();
    assert!(report.complete, "{report:?}");
    let entry = &ConversionManifest::load(&source.join("conversion-manifest.json"))
        .unwrap()
        .entries[diffuse];
    assert_eq!(
        hash_file(&output.join(&entry.output)).unwrap(),
        entry.output_hash
    );
}

/// A capability dropped during final refresh must affect both published completeness flags.
#[tokio::test]
async fn metadata_rebuild_completeness_includes_late_extension_warnings() {
    use shared::extensions::{ExtensionEntry, ExtensionHasher, ExtensionOwns};
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("rebuilt");
    generate(&data);
    convert(&data, &source).await;
    let foreign_path = "optional-payload.bin";
    let payload = b"source-owned optional payload";
    fs::write(source.join(foreign_path), payload).unwrap();
    let connection = Connection::open(source.join("skyrim_world.db")).unwrap();
    let core: String = connection
        .query_row(
            "SELECT build_identity FROM lod_build WHERE id=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    drop(connection);
    let mut hasher = ExtensionHasher::new("example.optional", 1);
    hasher.add_file(foreign_path, payload);
    let manifest_path = source.join("conversion-manifest.json");
    let mut manifest = ConversionManifest::load(&manifest_path).unwrap();
    manifest.extensions.push(ExtensionEntry {
        id: "example.optional".into(),
        version: 1,
        owns: ExtensionOwns {
            files: vec![foreign_path.into()],
            ..Default::default()
        },
        sha256: hasher.finish(),
        built_from: core,
        stats: Default::default(),
    });
    manifest.save(&manifest_path).unwrap();
    let source_hash = hash_file(&manifest_path).unwrap();
    let report = rebuild(&data, &source, &output).await.unwrap();
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("example.optional")),
        "late refresh warning was not exercised: {:?}",
        report.warnings
    );
    assert!(
        !report.complete,
        "late extension warning must mark the report incomplete"
    );
    let published = ConversionManifest::load(&output.join("conversion-manifest.json")).unwrap();
    assert!(
        !published.complete,
        "manifest and report must agree after final refresh"
    );
    assert!(
        !published
            .extensions
            .iter()
            .any(|entry| entry.id == "example.optional")
    );
    assert_eq!(
        hash_file(&manifest_path).unwrap(),
        source_hash,
        "rebuild must not modify the source manifest"
    );
    assert_eq!(fs::read(source.join(foreign_path)).unwrap(), payload);
}

/// Old complete packs can contain optional derived files and strays without output proofs.
/// Omit only those files while publishing every verified neighboring asset unchanged.
#[tokio::test]
async fn metadata_rebuild_skips_unproven_derived_outputs_and_strays_locally() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("rebuilt");
    generate(&data);
    convert(&data, &source).await;
    let manifest = ConversionManifest::load(&source.join("conversion-manifest.json")).unwrap();
    let manifest_hash = hash_file(&source.join("conversion-manifest.json")).unwrap();
    let before: Vec<_> = manifest
        .entries
        .values()
        .map(|entry| {
            (
                entry.output.clone(),
                hash_file(&source.join(&entry.output)).unwrap(),
            )
        })
        .collect();
    let unproven = [
        "meshes/actors/character/composed/default/male/body.glb",
        "meshes/actors/character/composed/default/female/body.glb",
        "meshes/actors/character/animations/male/mt_idle.glb",
        "meshes/actors/character/animations/female/mt_idle.glb",
        "scripts/mod-stray.luau",
        "textures/mod-stray.bin",
    ];
    for relative in unproven {
        let path = source.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"unproven bytes must never be retained").unwrap();
    }
    let result = rebuild(&data, &source, &output).await;
    assert!(
        result.is_ok(),
        "local unproven files aborted publication: {result:?}"
    );
    let report = result.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    assert!(report.warnings.is_empty());
    assert!(
        report
            .notices
            .iter()
            .any(|notice| notice.contains("6 unproven retained assets")),
        "{:?}",
        report.notices
    );
    for relative in unproven {
        assert!(
            !output.join(relative).exists(),
            "unproven output published: {relative}"
        );
        assert_eq!(
            fs::read(source.join(relative)).unwrap(),
            b"unproven bytes must never be retained"
        );
    }
    for (relative, hash) in before {
        assert_eq!(
            hash_file(&output.join(&relative)).unwrap(),
            hash,
            "verified neighbor changed: {relative}"
        );
        assert_eq!(hash_file(&source.join(&relative)).unwrap(), hash);
    }
    assert_eq!(
        hash_file(&source.join("conversion-manifest.json")).unwrap(),
        manifest_hash
    );
    let connection = Connection::open(output.join("skyrim_world.db")).unwrap();
    let count: u64 = connection
        .query_row(
            "SELECT COUNT(*) FROM \"references\" WHERE id=4660",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "valid neighboring plugin record must publish");
}

/// Supply independent synthetic producer facts for the consumer's body-recipe fixture.
fn derived_body_proof(bytes: &[u8]) -> converter::cache::DerivedOutput {
    converter::cache::DerivedOutput {
        recipe: "body-compose-v1".into(),
        inputs: [
            (
                "meshes/actors/character/character assets/malebody_1.nif",
                "body",
            ),
            (
                "meshes/actors/character/character assets/malehands_1.nif",
                "hands",
            ),
            (
                "meshes/actors/character/character assets/malefeet_1.nif",
                "feet",
            ),
            (
                "meshes/actors/character/character assets/skeleton.nif",
                "skeleton",
            ),
        ]
        .into_iter()
        .map(|(path, bytes)| (path.into(), converter::cache::hash_bytes(bytes.as_bytes())))
        .collect(),
        output_size: bytes.len() as u64,
        output_hash: converter::cache::hash_bytes(bytes),
    }
}

/// Hold a Windows source file open without sharing reads until the guard is dropped.
#[cfg(windows)]
fn block_file_reads(path: &Path) -> Option<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    Some(
        fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(path)
            .unwrap(),
    )
}

/// Restore a Unix fixture's permissions even if the rebuild or an assertion panics.
#[cfg(unix)]
struct RestoreFilePermissions<'a> {
    path: &'a Path,
    permissions: fs::Permissions,
}

#[cfg(unix)]
impl Drop for RestoreFilePermissions<'_> {
    /// Best-effort restoration must not hide the original assertion failure.
    fn drop(&mut self) {
        let _ = fs::set_permissions(self.path, self.permissions.clone());
    }
}

/// Deny Unix reads, skipping root or another process that bypasses file permissions.
#[cfg(unix)]
fn block_file_reads(path: &Path) -> Option<RestoreFilePermissions<'_>> {
    use std::os::unix::fs::PermissionsExt;
    let restore = RestoreFilePermissions {
        path,
        permissions: fs::metadata(path).unwrap().permissions(),
    };
    fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(path).is_ok() {
        eprintln!("skipping unreadable-file fixture: file permissions are bypassed (root)");
        return None;
    }
    Some(restore)
}

/// An unreadable optional body with a valid proof cannot block verified neighboring publication.
#[cfg(any(windows, unix))]
#[tokio::test]
async fn metadata_rebuild_omits_unreadable_proven_derived_output() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("rebuilt");
    generate(&data);
    convert(&data, &source).await;
    let manifest_path = source.join("conversion-manifest.json");
    let mut manifest = ConversionManifest::load(&manifest_path).unwrap();
    let neighbor = manifest.entries[layout::GENERATED_MODEL_PATH].clone();
    let neighbor_hash = hash_file(&source.join(&neighbor.output)).unwrap();
    let bytes = fs::read(source.join(&neighbor.output)).unwrap();
    let relative = "meshes/actors/character/composed/default/male/body.glb";
    let body = source.join(relative);
    fs::create_dir_all(body.parent().unwrap()).unwrap();
    fs::write(&body, &bytes).unwrap();
    manifest
        .derived_outputs
        .insert(relative.into(), derived_body_proof(&bytes));
    manifest.save(&manifest_path).unwrap();
    let source_manifest_hash = hash_file(&manifest_path).unwrap();
    let Some(unreadable) = block_file_reads(&body) else {
        return;
    };
    assert!(fs::read(&body).is_err(), "fixture must actually deny reads");
    let result = rebuild(&data, &source, &output).await;
    drop(unreadable);
    assert!(
        result.is_ok(),
        "unreadable optional derived output aborted publication: {result:?}"
    );
    let report = result.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    assert!(report.warnings.is_empty());
    let omissions: Vec<_> = report
        .notices
        .iter()
        .filter(|notice| notice.contains("unproven retained assets"))
        .collect();
    assert_eq!(omissions.len(), 1, "{:?}", report.notices);
    assert!(omissions[0].contains("1 unproven retained assets"));
    assert!(omissions[0].contains(relative));
    assert!(
        !output.join(relative).exists(),
        "partial body must not publish"
    );
    assert_eq!(
        hash_file(&output.join(&neighbor.output)).unwrap(),
        neighbor_hash
    );
    let published = ConversionManifest::load(&output.join("conversion-manifest.json")).unwrap();
    assert!(published.complete);
    assert!(published.derived_outputs.is_empty());
    assert_eq!(
        published.entries[layout::GENERATED_MODEL_PATH].output_hash,
        neighbor_hash
    );
    assert_eq!(fs::read(&body).unwrap(), bytes);
    assert_eq!(
        hash_file(&source.join(&neighbor.output)).unwrap(),
        neighbor_hash
    );
    assert_eq!(hash_file(&manifest_path).unwrap(), source_manifest_hash);
}

/// The optional-output recovery must never hide an unreadable ordinary recorded asset.
#[cfg(any(windows, unix))]
#[tokio::test]
async fn metadata_rebuild_rejects_unreadable_ordinary_asset() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("rebuilt");
    generate(&data);
    convert(&data, &source).await;
    let manifest = ConversionManifest::load(&source.join("conversion-manifest.json")).unwrap();
    let relative = &manifest.entries[layout::GENERATED_MODEL_PATH].output;
    let path = source.join(relative);
    let original_hash = hash_file(&path).unwrap();
    let Some(unreadable) = block_file_reads(&path) else {
        return;
    };
    assert!(fs::read(&path).is_err(), "fixture must actually deny reads");
    let result = rebuild(&data, &source, &output).await;
    drop(unreadable);
    let error = result.unwrap_err().to_string();
    assert!(
        error.contains(&format!("failed to copy retained asset {relative}")),
        "{error}"
    );
    assert!(
        !output.exists(),
        "unverified ordinary bytes must not publish"
    );
    assert_eq!(hash_file(&path).unwrap(), original_hash);
}

/// Unproven symlinks are omitted without following or modifying their external target.
#[cfg(unix)]
#[tokio::test]
async fn metadata_rebuild_skips_stray_symlinks_without_following_them() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("rebuilt");
    generate(&data);
    convert(&data, &source).await;
    let external = directory.path().join("protected.luau");
    fs::write(&external, b"protected external bytes").unwrap();
    std::os::unix::fs::symlink(&external, source.join("scripts/stray.luau")).unwrap();
    let report = rebuild(&data, &source, &output).await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    assert!(
        report
            .notices
            .iter()
            .any(|notice| notice.contains("1 unproven retained assets"))
    );
    assert!(!output.join("scripts/stray.luau").exists());
    assert_eq!(fs::read(&external).unwrap(), b"protected external bytes");
    assert!(
        source
            .join("scripts/stray.luau")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

/// Runtime-only packs preserve authenticated derived bytes and their original proof repeatedly.
#[tokio::test]
async fn metadata_rebuild_retains_proven_derived_bytes_without_raw_inputs() {
    for recipe in ["body-compose-v1", "body-compose-v2"] {
        assert_runtime_only_derived_reuse(recipe).await;
    }
}

/// Publishes each supported body recipe twice without consulting its original inputs.
async fn assert_runtime_only_derived_reuse(recipe: &str) {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    generate(&data);
    convert(&data, &source).await;
    assert!(!source.join("vfs").exists());
    let manifest_path = source.join("conversion-manifest.json");
    let mut manifest = ConversionManifest::load(&manifest_path).unwrap();
    let mesh = manifest
        .entries
        .values()
        .find(|entry| entry.output.ends_with(".glb"))
        .unwrap();
    let bytes = fs::read(source.join(&mesh.output)).unwrap();
    let relative = "meshes/actors/character/composed/default/male/body.glb";
    fs::create_dir_all(source.join(relative).parent().unwrap()).unwrap();
    fs::write(source.join(relative), &bytes).unwrap();
    // Moving the synthetic scene changes its relative texture base; finalize its
    // absent references before recording producer facts for the retained bytes.
    let pruned = converter::mesh::MeshConverter::prune_dangling_texture_uris(&source).unwrap();
    assert_eq!(pruned.len(), 1);
    assert_eq!(pruned[0].glb, relative);
    let bytes = fs::read(source.join(relative)).unwrap();
    let mut proof = derived_body_proof(&bytes);
    proof.recipe = recipe.into();
    if recipe == "body-compose-v2" {
        proof.inputs.insert(
            "recipes/body-compose-v2/context.json".into(),
            hash_bytes(b"[[[\"body\",null],[\"hands\",null],[\"feet\",null]],[]]"),
        );
    }
    manifest
        .derived_outputs
        .insert(relative.into(), proof.clone());
    manifest.save(&manifest_path).unwrap();
    let source_hash = hash_file(&manifest_path).unwrap();
    let mut previous = source.clone();
    for name in ["rebuilt", "repeated"] {
        let output = directory.path().join(name);
        let report = rebuild(&data, &previous, &output).await.unwrap();
        assert!(report.complete, "{:?}", report.warnings);
        assert!(
            !report
                .notices
                .iter()
                .any(|notice| notice.contains("unproven retained"))
        );
        assert_eq!(fs::read(output.join(relative)).unwrap(), bytes);
        let current = ConversionManifest::load(&output.join("conversion-manifest.json")).unwrap();
        assert_eq!(current.derived_outputs.get(relative), Some(&proof));
        let catalog: serde_json::Value =
            serde_json::from_slice(&fs::read(output.join("streaming-costs.json")).unwrap())
                .unwrap();
        assert!(catalog.to_string().contains(relative));
        previous = output;
    }
    assert_eq!(hash_file(&manifest_path).unwrap(), source_hash);
}

/// Malformed, unsupported, missing and corrupted optional outputs cannot block valid neighbors.
#[tokio::test]
async fn metadata_rebuild_omits_invalid_individual_derived_proofs() {
    for case in [
        "recipe",
        "size",
        "hash",
        "inputs",
        "missing",
        "malformed",
        "malformed-missing",
        "escaping",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let source = directory.path().join("source");
        let output = directory.path().join("rebuilt");
        generate(&data);
        convert(&data, &source).await;
        let manifest_path = source.join("conversion-manifest.json");
        let manifest = ConversionManifest::load(&manifest_path).unwrap();
        let neighbor = manifest
            .entries
            .values()
            .find(|entry| entry.output.ends_with(".glb"))
            .unwrap();
        let neighbor_hash = hash_file(&source.join(&neighbor.output)).unwrap();
        let bytes = fs::read(source.join(&neighbor.output)).unwrap();
        let relative = "meshes/actors/character/composed/default/male/body.glb";
        let mut proof = serde_json::to_value(derived_body_proof(&bytes)).unwrap();
        match case {
            "recipe" => proof["recipe"] = "future-body-v99".into(),
            "size" => proof["output_size"] = 1.into(),
            "hash" => proof["output_hash"] = "0".repeat(64).into(),
            "inputs" => proof["inputs"] = serde_json::json!({"wrong.nif": "0".repeat(64)}),
            "malformed" | "malformed-missing" => proof["output_size"] = "not an integer".into(),
            _ => {}
        }
        let mut document = serde_json::to_value(&manifest).unwrap();
        let proof_path = if case == "escaping" {
            "../escaped.glb"
        } else {
            relative
        };
        document["derived_outputs"] = serde_json::json!({proof_path: proof});
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&document).unwrap(),
        )
        .unwrap();
        if !["missing", "malformed-missing", "escaping"].contains(&case) {
            fs::create_dir_all(source.join(relative).parent().unwrap()).unwrap();
            fs::write(source.join(relative), &bytes).unwrap();
        }
        let source_hash = hash_file(&manifest_path).unwrap();
        let result = rebuild(&data, &source, &output).await;
        assert!(result.is_ok(), "{case}: {result:?}");
        let report = result.unwrap();
        assert!(report.complete, "{case}: {:?}", report.warnings);
        assert_eq!(
            report
                .notices
                .iter()
                .filter(|notice| notice.contains("1 unproven retained assets"))
                .count(),
            1,
            "{case}: {:?}",
            report.notices
        );
        assert!(!output.join(relative).exists(), "{case}");
        assert!(!directory.path().join("escaped.glb").exists());
        assert_eq!(
            hash_file(&output.join(&neighbor.output)).unwrap(),
            neighbor_hash,
            "{case}"
        );
        assert!(
            ConversionManifest::load(&output.join("conversion-manifest.json"))
                .unwrap()
                .derived_outputs
                .is_empty(),
            "{case}"
        );
        assert_eq!(hash_file(&manifest_path).unwrap(), source_hash, "{case}");
    }
}

/// Adds a scoped volcanic model whose authored normal is absent but DLC02 source exists.
fn add_volcanic_source(data: &Path) {
    let model = data.join(VOLCANIC_MODEL);
    fs::create_dir_all(model.parent().unwrap()).unwrap();
    fs::write(
        model,
        nif::static_shape(&nif::StaticShape {
            name: "VolcanicReuseTriangle",
            positions: &[[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [0.0, 3.0, 0.0]],
            normals: &[[0.0, 0.0, 1.0]; 3],
            uvs: &[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
            indices: &[[0, 1, 2]],
            diffuse: layout::GENERATED_DIFFUSE_PATH,
            normal_texture: VOLCANIC_ORIGINAL,
        })
        .unwrap(),
    )
    .unwrap();
    let target = data.join(VOLCANIC_TARGET);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::copy(data.join(layout::GENERATED_NORMAL_PATH), target).unwrap();
}

/// Reads the external normal URI without reproducing the repair's path mapping.
fn volcanic_normal_uri(glb: &Path) -> Option<String> {
    converter::mesh::MeshConverter::glb_texture_dependencies(glb)
        .unwrap()
        .into_iter()
        .find(|dependency| dependency.uri.contains("volcanicashgrass02_n"))
        .map(|dependency| dependency.uri)
}

/// Frame the light's enable-parent target in the generated worldspace's first exterior cell.
fn enable_parent_target() -> Vec<u8> {
    let payload = [
        b"NAME".as_slice(),
        &4u16.to_le_bytes(),
        &3u32.to_le_bytes(),
        b"DATA".as_slice(),
        &24u16.to_le_bytes(),
        &[0; 24],
    ]
    .concat();
    let reference = [
        b"REFR".as_slice(),
        &(payload.len() as u32).to_le_bytes(),
        &0u32.to_le_bytes(),
        &0x1234u32.to_le_bytes(),
        &0u32.to_le_bytes(),
        &44u16.to_le_bytes(),
        &0u16.to_le_bytes(),
        &payload,
    ]
    .concat();
    // The dummy writer assigns worldspace 1 and its first exterior CELL 0x10.
    // Preserve both parent contexts while adding a separate cell-children group.
    let cell_children = [
        b"GRUP".as_slice(),
        &(reference.len() as u32 + 24).to_le_bytes(),
        &0x10u32.to_le_bytes(),
        &8i32.to_le_bytes(),
        &[0; 8],
        &reference,
    ]
    .concat();
    [
        b"GRUP".as_slice(),
        &(cell_children.len() as u32 + 24).to_le_bytes(),
        &1u32.to_le_bytes(),
        &1i32.to_le_bytes(),
        &[0; 8],
        &cell_children,
    ]
    .concat()
}

/// Encode a synthetic plugin subrecord without depending on exporter internals.
fn grass_subrecord(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    [
        tag.as_slice(),
        &(payload.len() as u16).to_le_bytes(),
        payload,
    ]
    .concat()
}

/// Encode a synthetic uncompressed record with its plugin-local FormID.
fn grass_record(tag: &[u8; 4], id: u32, payload: Vec<u8>) -> Vec<u8> {
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

/// Add a mod-owned grass rule to the generated terrain's winning texture override.
fn add_grass_plugin(data: &Path) {
    let header = [
        grass_subrecord(b"MAST", b"Skyrim.esm\0"),
        grass_subrecord(b"DATA", &[0; 8]),
    ]
    .concat();
    let mut rule = vec![37, 0, 90, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    for value in [12.0f32, 0.25, 0.5, 1.75] {
        rule.extend(value.to_le_bytes());
    }
    rule.extend([0; 4]);
    let model = format!("{}\0", layout::GENERATED_MODEL_PATH);
    let grass = grass_record(
        b"GRAS",
        0x0100_0801,
        [
            grass_subrecord(b"EDID", b"RebuildGrass\0"),
            grass_subrecord(b"MODL", model.as_bytes()),
            grass_subrecord(b"DATA", &rule),
        ]
        .concat(),
    );
    let texture = grass_record(
        b"LTEX",
        4,
        [
            grass_subrecord(b"TNAM", &2u32.to_le_bytes()),
            grass_subrecord(b"GNAM", &0x0100_0801u32.to_le_bytes()),
        ]
        .concat(),
    );
    fs::write(
        data.join("Grass.esp"),
        [grass_record(b"TES4", 0, header), grass, texture].concat(),
    )
    .unwrap();
}

/// Rebuilding metadata preserves a nonempty grass archive and reuses source cells.
#[tokio::test]
async fn metadata_rebuild_preserves_grass_cache_and_reuses_unchanged_cells() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("derived");
    generate(&data);
    add_grass_plugin(&data);
    convert(&data, &source).await;
    let source_cache = source.join("grass_cache.rkyv");
    let original_hash = hash_file(&source_cache).unwrap();
    let original_manifest = hash_file(&source.join("conversion-manifest.json")).unwrap();
    let map = converter::esm::grass_cache::validate_grass_cache(&source_cache).unwrap();
    let cache =
        rkyv::access::<shared::grass::ArchivedGrassCache, rkyv::rancor::Error>(&map).unwrap();
    assert_eq!(cache.types.len(), 1);
    assert_eq!(cache.types[0].form_id.to_native(), 0x0100_0801);
    assert_eq!(
        cache.cells.len(),
        1,
        "fixture must contain grass-bearing terrain"
    );
    let cell_count = cache.cells.len();
    assert!(cache.cells[0].quadrants.iter().any(|quadrant| {
        quadrant
            .layers
            .iter()
            .any(|layer| !layer.grass_ids.is_empty())
    }));
    drop(map);
    let report = rebuild(&data, &source, &output).await.unwrap();
    let rebuilt_cache = output.join("grass_cache.rkyv");
    assert!(
        rebuilt_cache.is_file(),
        "metadata rebuild must publish the grass cache"
    );
    converter::esm::grass_cache::validate_grass_cache(&rebuilt_cache).unwrap();
    assert!(report.complete);
    assert!(report.artifacts.contains(&"grass_cache.rkyv".into()));
    assert_eq!(hash_file(&rebuilt_cache).unwrap(), original_hash);
    assert_eq!(hash_file(&source_cache).unwrap(), original_hash);
    assert_eq!(
        hash_file(&source.join("conversion-manifest.json")).unwrap(),
        original_manifest
    );
    let provenance: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("metadata-rebuild.json")).unwrap()).unwrap();
    assert_eq!(provenance["grass_baked_cells"], 0);
    assert_eq!(provenance["grass_reused_cells"], cell_count);
}

/// Append an explicitly excluded sibling without changing the visible primitive or binary data.
fn add_broken_marker(glb_path: &Path, original: &[u8], alpha_mode: serde_json::Value) {
    let glb = gltf::binary::Glb::from_slice(original).unwrap();
    let mut document: serde_json::Value = serde_json::from_slice(&glb.json).unwrap();
    let materials = document["materials"].as_array_mut().unwrap();
    let marker_material = materials.len();
    materials.push(serde_json::json!({
        "alphaMode": alpha_mode,
        "alphaCutoff": 1.0,
        "pbrMetallicRoughness": {"baseColorFactor": [0.0, 0.0, 0.0, 0.0]},
        "extras": {"openSkyrim": {"nonRenderingExclusion": true}}
    }));
    let primitives = document["meshes"][0]["primitives"].as_array_mut().unwrap();
    let mut marker = primitives[0].clone();
    marker["material"] = serde_json::json!(marker_material);
    marker["extras"] = serde_json::json!({
        "openSkyrim": {"materialExclusion": "BSX EditorMarker"}
    });
    primitives.push(marker);
    let mut json = serde_json::to_vec(&document).unwrap();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let bin = glb.bin.unwrap();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"glTF");
    bytes.extend(2u32.to_le_bytes());
    bytes.extend(((28 + json.len() + bin.len()) as u32).to_le_bytes());
    bytes.extend((json.len() as u32).to_le_bytes());
    bytes.extend_from_slice(b"JSON");
    bytes.extend(json);
    bytes.extend((bin.len() as u32).to_le_bytes());
    bytes.extend_from_slice(b"BIN\0");
    bytes.extend_from_slice(&bin);
    fs::write(glb_path, bytes).unwrap();
}

/// Publish healthy records and same-model obstruction despite a malformed excluded material.
#[tokio::test]
async fn metadata_rebuild_preserves_visible_obstruction_beside_broken_marker() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    generate(&data);
    let plugin = esm::Plugin {
        cells: &[
            esm::PRESET_EXTERIOR_CELL,
            esm::Cell {
                grid_x: 1,
                grid_y: 0,
            },
        ],
        author: layout::GENERATED_AUTHOR,
        worldspace: layout::GENERATED_WORLDSPACE,
        model_path: layout::GENERATED_MODEL_PATH,
        diffuse: layout::GENERATED_DIFFUSE_PATH,
        normal_texture: layout::GENERATED_NORMAL_PATH,
    };
    let light = esm::Light {
        model_path: None,
        reference_flags: 0x800,
        enable_parent: Some((0x1234, 1)),
        ..esm::PRESET_LIGHT
    };
    let mut plugin_bytes = esm::plugin_with_lights(&plugin, &light).unwrap();
    plugin_bytes.extend(enable_parent_target());
    fs::write(data.join("Skyrim.esm"), plugin_bytes).unwrap();
    add_grass_plugin(&data);
    // Each static is at cell-local (2048, 2048). This elevated horizontal
    // triangle covers the northeast quadrant's first sample at (2064, 2064).
    fs::write(
        data.join(layout::GENERATED_MODEL_PATH),
        nif::static_shape(&nif::StaticShape {
            name: "VisibleObstruction",
            positions: &[
                [0.0, 0.0, 128.0],
                [1024.0, 0.0, 128.0],
                [0.0, 1024.0, 128.0],
            ],
            normals: &[[0.0, 0.0, 1.0]; 3],
            uvs: &[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
            indices: &[[0, 1, 2]],
            diffuse: layout::GENERATED_DIFFUSE_PATH,
            normal_texture: layout::GENERATED_NORMAL_PATH,
        })
        .unwrap(),
    )
    .unwrap();
    convert(&data, &source).await;
    let source_cache = source.join("grass_cache.rkyv");
    let source_cache_hash = hash_file(&source_cache).unwrap();
    let map = converter::esm::grass_cache::validate_grass_cache(&source_cache).unwrap();
    let control =
        rkyv::access::<shared::grass::ArchivedGrassCache, rkyv::rancor::Error>(&map).unwrap();
    assert_eq!(control.cells.len(), 2);
    assert_eq!(control.cells[0].cell_id.to_native(), 0x10);
    assert_eq!(control.cells[1].cell_id.to_native(), 0x20);
    let expected_masks: Vec<_> = control
        .cells
        .iter()
        .map(|cell| {
            assert_eq!(
                cell.quadrants[3].exclusion[0] & 1,
                1,
                "visible control must block"
            );
            (
                cell.cell_id.to_native(),
                cell.quadrants
                    .iter()
                    .map(|quadrant| quadrant.exclusion.to_vec())
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    drop(map);
    let manifest_path = source.join("conversion-manifest.json");
    let original_manifest = ConversionManifest::load(&manifest_path).unwrap();
    let glb_path = source.join(&original_manifest.entries[layout::GENERATED_MODEL_PATH].output);
    let original_glb = fs::read(&glb_path).unwrap();
    for (case, alpha_mode) in [serde_json::json!("NOT_A_MODE"), serde_json::json!(17)]
        .into_iter()
        .enumerate()
    {
        add_broken_marker(&glb_path, &original_glb, alpha_mode);
        // Keep retention proofs valid: this tests obstruction recovery, not checksum rejection.
        let glb_hash = hash_file(&glb_path).unwrap();
        let mut manifest = original_manifest.clone();
        let entry = manifest
            .entries
            .get_mut(layout::GENERATED_MODEL_PATH)
            .unwrap();
        entry.output_hash = glb_hash.clone();
        entry.output_size = fs::metadata(&glb_path).unwrap().len();
        manifest.save(&manifest_path).unwrap();
        let source_manifest_hash = hash_file(&manifest_path).unwrap();
        let output = directory.path().join(format!("derived-marker-{case}"));
        let result = rebuild(&data, &source, &output).await;
        assert!(
            result.is_ok(),
            "excluded marker aborted publication: {result:?}"
        );
        let report = result.unwrap();
        assert!(report.complete, "{:?}", report.warnings);
        assert!(report.artifacts.contains(&"grass_cache.rkyv".into()));
        let connection = Connection::open(output.join("skyrim_world.db")).unwrap();
        converter::esm::exporter::validate_database(&connection).unwrap();
        for id in [0x12, 0x22, 0x1234] {
            let count: u64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM \"references\" WHERE id=?1",
                    [id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "valid neighboring reference {id:#x} must publish");
        }
        let published_manifest =
            ConversionManifest::load(&output.join("conversion-manifest.json")).unwrap();
        assert!(published_manifest.complete);
        assert_eq!(
            published_manifest.entries[layout::GENERATED_MODEL_PATH].output_hash,
            glb_hash
        );
        assert_eq!(
            hash_file(&output.join(&manifest.entries[layout::GENERATED_MODEL_PATH].output))
                .unwrap(),
            glb_hash
        );
        assert_eq!(hash_file(&source_cache).unwrap(), source_cache_hash);
        assert_eq!(hash_file(&glb_path).unwrap(), glb_hash);
        assert_eq!(hash_file(&manifest_path).unwrap(), source_manifest_hash);
        let provenance: serde_json::Value =
            serde_json::from_slice(&fs::read(output.join("metadata-rebuild.json")).unwrap())
                .unwrap();
        assert_eq!(provenance["grass_baked_cells"], 2);
        assert_eq!(provenance["grass_reused_cells"], 0);
        let rebuilt = output.join("grass_cache.rkyv");
        let map = converter::esm::grass_cache::validate_grass_cache(&rebuilt).unwrap();
        let cache =
            rkyv::access::<shared::grass::ArchivedGrassCache, rkyv::rancor::Error>(&map).unwrap();
        assert_eq!(cache.types.len(), 1);
        assert_eq!(cache.types[0].form_id.to_native(), 0x0100_0801);
        assert_eq!(
            cache.cells.len(),
            2,
            "healthy neighboring terrain must publish"
        );
        for (id, masks) in &expected_masks {
            let cell = cache
                .cells
                .iter()
                .find(|cell| cell.cell_id.to_native() == *id)
                .unwrap();
            assert!(cell.quadrants.iter().any(|quadrant| {
                quadrant.layers.iter().any(|layer| {
                    layer
                        .grass_ids
                        .iter()
                        .any(|id| id.to_native() == 0x0100_0801)
                })
            }));
            assert_eq!(
                cell.quadrants[3].exclusion[0] & 1,
                1,
                "excluded marker lost the visible sibling's obstruction in cell {id:#x}"
            );
            let actual: Vec<_> = cell
                .quadrants
                .iter()
                .map(|quadrant| quadrant.exclusion.to_vec())
                .collect();
            assert_eq!(
                actual, *masks,
                "excluded marker lost the visible sibling's obstruction in cell {id:#x}"
            );
            assert!(
                cell.obstruction_failures.is_empty(),
                "excluded marker poisoned its model"
            );
        }
    }
}

/// Generate complete assets and a light whose enable-parent resolves to an authored reference.
fn generate(data: &Path) {
    layout::prepare_directory(data, false).unwrap();
    layout::generate(
        data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,nif,pex,esm,lodsettings").unwrap(),
    )
    .unwrap();
    let plugin = esm::Plugin {
        author: layout::GENERATED_AUTHOR,
        worldspace: layout::GENERATED_WORLDSPACE,
        cells: &[esm::PRESET_EXTERIOR_CELL],
        model_path: layout::GENERATED_MODEL_PATH,
        diffuse: layout::GENERATED_DIFFUSE_PATH,
        normal_texture: layout::GENERATED_NORMAL_PATH,
    };
    let light = esm::Light {
        model_path: None,
        reference_flags: 0x800,
        enable_parent: Some((0x1234, 1)),
        ..esm::PRESET_LIGHT
    };
    let mut plugin_bytes = esm::plugin_with_lights(&plugin, &light).unwrap();
    plugin_bytes.extend(enable_parent_target());
    fs::write(data.join("Skyrim.esm"), plugin_bytes).unwrap();
    fs::write(
        data.join("Skyrim - Misc.bsa"),
        bsa::v105(
            &[
                Entry {
                    name: "LODSettings/GeneratedWorld.LOD",
                    data: &PACKED_SETTINGS,
                },
                Entry {
                    name: "misc/unrelated.txt",
                    data: b"not a setting",
                },
            ],
            bsa::Compression::None,
        )
        .unwrap(),
    )
    .unwrap();
}

async fn convert(data: &Path, output: &Path) -> converter::PipelineReport {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(common::cpu_lod_config(data, output), tx)
        .await
        .unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    report
}

async fn rebuild(
    data: &Path,
    source: &Path,
    output: &Path,
) -> color_eyre::Result<converter::PipelineReport> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let result =
        AssetPipeline::rebuild_metadata_async(common::cpu_lod_config(data, output), source, tx)
            .await;
    drain.await.unwrap();
    result
}

/// Recreate the legacy schema-15 producer proof alongside its historical database shape.
fn schema_four_source(source: &Path) {
    fs::write(source.join("record-reader.json"), br#"{"mode":"legacy"}"#).unwrap();
    let mut manifest: ConversionManifest =
        serde_json::from_slice(&fs::read(source.join("conversion-manifest.json")).unwrap())
            .unwrap();
    manifest.schema_version = 15;
    manifest.asset_configuration_hashes.clear();
    let mut legacy_config = PipelineConfig::new(".", ".");
    legacy_config.record_reader = converter::config::RecordReader::Legacy;
    manifest.configuration_hash =
        converter::cache::configuration_hash_for_schema(&legacy_config, 15).unwrap();
    manifest
        .save(&source.join("conversion-manifest.json"))
        .unwrap();
    let connection = Connection::open(source.join("skyrim_world.db")).unwrap();
    connection
        .execute_batch(
            "UPDATE schema_info SET version=4;
        ALTER TABLE \"references\" DROP COLUMN header_flags;
        ALTER TABLE \"references\" DROP COLUMN enable_parent_id;
        ALTER TABLE \"references\" DROP COLUMN enable_parent_flags;
        ALTER TABLE worldspaces DROP COLUMN lod_origin_x;
        ALTER TABLE worldspaces DROP COLUMN lod_origin_y;",
        )
        .unwrap();
}

fn native_configuration_hash(config: &PipelineConfig, zstd_level: u32) -> String {
    converter::cache::hash_bytes(
        &serde_json::to_vec(&serde_json::json!({
            "schema": 16,
            "texture_etc1s_quality": config.texture_fallback_quality,
            "texture_uastc_level": config.texture_uastc_level,
            "texture_zstd_level": zstd_level,
            "script_abi_version": config.script_abi_version,
        }))
        .unwrap(),
    )
}

/// A schema-16 native producer cannot be relabeled as current by metadata rebuild: its KTX2
/// textures predate the current layout, so the source must be regenerated by normal conversion.
#[tokio::test]
async fn native_pre_producer_28_configuration_is_rejected_not_relabeled() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("derived");
    generate(&data);
    convert(&data, &source).await;
    let mut config = common::cpu_lod_config(&data, &output);
    config.script_abi_version = 1; // The literal schema-16 producer proof predates ABI 2.
    let producer_hash = native_configuration_hash(&config, 6);
    assert_eq!(
        producer_hash,
        "ebc4fe2f7d531e796e6a5e22b70a1f6a877c01e963b54b416b789d0f64312427"
    );
    let path = source.join("conversion-manifest.json");
    let mut manifest: ConversionManifest =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    manifest.schema_version = 16;
    manifest.asset_configuration_hashes.clear();
    manifest.configuration_hash = producer_hash;
    manifest.save(&path).unwrap();
    fs::write(source.join("record-reader.json"), br#"{"mode":"legacy"}"#).unwrap();
    let error = rebuild(&data, &source, &output).await.unwrap_err();
    assert!(
        format!("{error:?}").contains("predate the current texture layout"),
        "{error:?}"
    );
    assert!(!output.exists());
}

/// Changed native settings fail against the exact recorded legacy producer identity.
#[tokio::test]
async fn metadata_rebuild_rejects_changed_native_producer_settings() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    generate(&data);
    convert(&data, &source).await;
    let path = source.join("conversion-manifest.json");
    let mut manifest: ConversionManifest =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for (index, retained) in [false, true].into_iter().enumerate() {
        let output = directory.path().join(format!("rejected-{index}"));
        let changed = native_configuration_hash(&common::cpu_lod_config(&data, &output), 7);
        if retained {
            manifest.schema_version = converter::cache::CONVERTER_SCHEMA_VERSION;
            let mut legacy_config = PipelineConfig::new(&data, &output);
            legacy_config.record_reader = converter::config::RecordReader::Legacy;
            manifest.configuration_hash =
                converter::cache::configuration_hash(&legacy_config).unwrap();
            manifest.retained_mesh_schema_version =
                Some(converter::cache::CONVERTER_SCHEMA_VERSION);
            manifest.asset_configuration_hashes.clear();
            manifest.retained_asset_configuration_hash = Some(changed);
            fs::write(
                source.join("record-reader.json"),
                serde_json::to_vec(&converter::esm::inhouse::reader_identity(
                    converter::config::RecordReader::Legacy,
                ))
                .unwrap(),
            )
            .unwrap();
            fs::write(
                source.join("metadata-rebuild.json"),
                br#"{"retained_asset_record_reader":{"mode":"legacy"}}"#,
            )
            .unwrap();
        } else {
            manifest.schema_version = 16;
            manifest.asset_configuration_hashes.clear();
            manifest.configuration_hash = changed;
            fs::write(source.join("record-reader.json"), br#"{"mode":"legacy"}"#).unwrap();
        }
        manifest.save(&path).unwrap();
        let error = rebuild(&data, &source, &output).await.unwrap_err();
        let expected = if retained {
            "retained producer configuration does not match rebuild settings"
        } else {
            "predate the current texture layout"
        };
        assert!(format!("{error:?}").contains(expected), "{error:?}");
        assert!(!output.exists());
    }
}

#[tokio::test]
async fn metadata_rebuild_reuses_bytes_and_recovers_authoritative_flags() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("derived");
    generate(&data);
    convert(&data, &source).await;
    let old_manifest = hash_file(&source.join("conversion-manifest.json")).unwrap();
    let old_db = hash_file(&source.join("skyrim_world.db")).unwrap();
    let source_manifest: ConversionManifest =
        serde_json::from_slice(&fs::read(source.join("conversion-manifest.json")).unwrap())
            .unwrap();
    let report = rebuild(&data, &source, &output).await.unwrap();
    assert!(report.complete);
    assert_eq!(report.converted, 0);
    assert!(report.cache_hits > 0 && report.lod_chunks > 0);
    assert_eq!(
        hash_file(&source.join("conversion-manifest.json")).unwrap(),
        old_manifest
    );
    assert_eq!(hash_file(&source.join("skyrim_world.db")).unwrap(), old_db);
    for entry in source_manifest.entries.values() {
        assert_eq!(
            hash_file(&output.join(&entry.output)).unwrap(),
            entry.output_hash
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_ne!(
                fs::metadata(source.join(&entry.output)).unwrap().ino(),
                fs::metadata(output.join(&entry.output)).unwrap().ino()
            );
        }
    }
    let connection = Connection::open(output.join("skyrim_world.db")).unwrap();
    let flags: (u32, u32, u32) = connection.query_row(
        "SELECT header_flags, enable_parent_id, enable_parent_flags FROM \"references\" WHERE header_flags=2048", [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap();
    assert_eq!(flags, (0x800, 0x1234, 1));
    let version: u32 = connection
        .query_row("SELECT version FROM schema_info", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, shared::WORLD_DATABASE_SCHEMA_VERSION);
    let provenance: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("metadata-rebuild.json")).unwrap()).unwrap();
    assert_eq!(
        provenance["source_converter_schema"],
        converter::cache::CONVERTER_SCHEMA_VERSION
    );
    assert_eq!(
        provenance["source_retained_mesh_schema"],
        converter::cache::CONVERTER_SCHEMA_VERSION
    );
    assert_eq!(provenance["source_manifest_hash"], old_manifest);
    assert_eq!(
        hash_file(&output.join("metadata-source-conversion-manifest.json")).unwrap(),
        old_manifest
    );
    assert!(output.join("lod-manifest.json").is_file());
}

/// Metadata output from a current-producer source keeps its GLBs as normal cache hits, and a
/// repeated rebuild regenerates nothing that the normal route would not regenerate.
#[tokio::test]
async fn metadata_rebuild_of_current_source_keeps_meshes_as_cache_hits() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("derived");
    let repeated = directory.path().join("derived-again");
    generate(&data);
    convert(&data, &source).await;
    rebuild(&data, &source, &output).await.unwrap();
    rebuild(&data, &output, &repeated).await.unwrap();
    let retained: ConversionManifest =
        serde_json::from_slice(&fs::read(repeated.join("conversion-manifest.json")).unwrap())
            .unwrap();
    assert_eq!(
        retained.retained_mesh_schema_version,
        Some(converter::cache::CONVERTER_SCHEMA_VERSION)
    );
    let meshes = retained
        .entries
        .values()
        .filter(|entry| entry.output.ends_with(".glb"))
        .count();
    assert!(meshes > 0);
    let eligible = ConversionManifest::load(&repeated.join("conversion-manifest.json")).unwrap();
    assert_eq!(
        eligible
            .entries
            .values()
            .filter(|entry| entry.output.ends_with(".glb"))
            .count(),
        meshes,
        "current-producer GLBs remain reusable"
    );
    let report = convert(&data, &repeated).await;
    let current: ConversionManifest =
        serde_json::from_slice(&fs::read(repeated.join("conversion-manifest.json")).unwrap())
            .unwrap();
    let extracted = current
        .archives
        .values()
        .map(|archive| archive.files.len())
        .sum::<usize>();
    assert_eq!(report.converted, extracted as u64);
    assert_eq!(
        report.cache_hits,
        retained
            .entries
            .values()
            .filter(|entry| entry.output != "grass_cache.rkyv")
            .count() as u64
    );
    assert_eq!(current.retained_mesh_schema_version, None);
}

/// Invalid source/retained producer pairs are stamped faithfully and rejected.
#[tokio::test]
async fn metadata_rebuild_rejects_unsupported_mesh_provenance() {
    // Only producer-27 meshes may survive into the current schema; producers 24 and 26 never do.
    let current = converter::cache::CONVERTER_SCHEMA_VERSION;
    for (schema, mesh_schema) in [
        (current, 24),
        (current, 26),
        (current, 25),
        (current, current + 1),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let source = directory.path().join("source");
        let output = directory.path().join("derived");
        generate(&data);
        convert(&data, &source).await;
        if schema == 15 {
            schema_four_source(&source);
        }
        let mut manifest: ConversionManifest =
            serde_json::from_slice(&fs::read(source.join("conversion-manifest.json")).unwrap())
                .unwrap();
        manifest.schema_version = schema;
        manifest.retained_mesh_schema_version = Some(mesh_schema);
        manifest.asset_configuration_hashes.clear();
        manifest
            .save(&source.join("conversion-manifest.json"))
            .unwrap();
        let error = rebuild(&data, &source, &output)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("unsupported retained mesh cache contract"),
            "{error}"
        );
        assert!(!output.exists());
    }
}

#[tokio::test]
async fn markerless_legacy_metadata_never_promotes_retained_meshes() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let legacy = directory.path().join("legacy-derived");
    let output = directory.path().join("derived-again");
    generate(&data);
    convert(&data, &source).await;
    rebuild(&data, &source, &legacy).await.unwrap();
    let manifest_path = legacy.join("conversion-manifest.json");
    let mut manifest: ConversionManifest =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest.retained_mesh_schema_version = None;
    manifest.save(&manifest_path).unwrap();
    fs::remove_file(legacy.join("metadata-source-conversion-manifest.json")).unwrap();
    let eligible = ConversionManifest::load(&manifest_path).unwrap();
    assert!(
        eligible
            .entries
            .values()
            .all(|entry| !entry.output.ends_with(".glb")),
        "markerless metadata-only output must not become a mesh cache hit"
    );
    let error = rebuild(&data, &legacy, &output)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("missing retained mesh provenance"),
        "{error}"
    );
    assert!(!output.exists());
}

#[tokio::test]
async fn metadata_rebuild_rejects_changed_plugins_and_assets_without_publication() {
    for corrupt_plugin in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let source = directory.path().join("source");
        let output = directory.path().join("derived");
        generate(&data);
        convert(&data, &source).await;
        if corrupt_plugin {
            fs::write(data.join("Skyrim.esm"), b"different plugin").unwrap();
        } else {
            let manifest =
                ConversionManifest::load(&source.join("conversion-manifest.json")).unwrap();
            let asset = &manifest
                .entries
                .values()
                .find(|entry| entry.output.starts_with("meshes/"))
                .unwrap()
                .output;
            fs::write(source.join(asset), b"corrupt asset").unwrap();
        }
        let error = rebuild(&data, &source, &output)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("checksum mismatch"), "{error}");
        assert!(!output.exists());
        assert!(!fs::read_dir(directory.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("staging")
        }));
    }
}

#[tokio::test]
async fn metadata_rebuild_requires_new_disjoint_output() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    generate(&data);
    convert(&data, &source).await;
    let original = hash_file(&source.join("conversion-manifest.json")).unwrap();
    for output in [&source, &source.join("nested"), &data.join("nested")] {
        assert!(rebuild(&data, &source, output).await.is_err());
    }
    let existing = directory.path().join("existing");
    fs::create_dir(&existing).unwrap();
    fs::write(existing.join("sentinel"), b"keep").unwrap();
    assert!(rebuild(&data, &source, &existing).await.is_err());
    assert_eq!(fs::read(existing.join("sentinel")).unwrap(), b"keep");
    assert_eq!(
        hash_file(&source.join("conversion-manifest.json")).unwrap(),
        original
    );
}

#[tokio::test]
async fn metadata_rebuild_preserves_package_snapshot_after_unrelated_data_asset_changes() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("derived");
    generate(&data);
    convert(&data, &source).await;
    let manifest: ConversionManifest =
        serde_json::from_slice(&fs::read(source.join("conversion-manifest.json")).unwrap())
            .unwrap();
    fs::write(
        data.join(layout::GENERATED_MODEL_PATH),
        b"changed NIF override",
    )
    .unwrap();
    fs::write(
        data.join(layout::GENERATED_NORMAL_PATH),
        b"changed normal DDS override",
    )
    .unwrap();
    fs::write(data.join("scripts/generated.pex"), b"changed PEX override").unwrap();
    fs::write(data.join("meshes/new.nif"), b"new asset").unwrap();
    let report = rebuild(&data, &source, &output).await.unwrap();
    assert!(report.complete);
    assert_eq!(report.converted, 0);
    for entry in manifest.entries.values() {
        assert_eq!(
            hash_file(&output.join(&entry.output)).unwrap(),
            entry.output_hash
        );
        assert_eq!(
            hash_file(&source.join(&entry.output)).unwrap(),
            entry.output_hash
        );
    }
    assert!(!output.join("meshes/new.glb").exists());
}

#[tokio::test]
async fn metadata_rebuild_resolves_current_packed_settings_and_omits_stale_payloads() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("derived");
    generate(&data);
    let model = layout::GENERATED_MODEL_PATH;
    let runtime = fs::read(data.join(model)).unwrap();
    let authoring = format!("CalienteTools/{model}");
    fs::write(
        data.join("Skyrim - Meshes.bsa"),
        bsa::v105(
            &[
                Entry::new(model, &runtime),
                Entry::new(&authoring, b"authoring mesh"),
            ],
            bsa::Compression::None,
        )
        .unwrap(),
    )
    .unwrap();
    convert(&data, &source).await;
    fs::remove_file(data.join("lodsettings/GeneratedWorld.lod")).unwrap();
    fs::write(
        data.join("Unmatched - Misc.bsa"),
        bsa::v105(
            &[Entry::new(
                "LODSettings/GeneratedWorld.LOD",
                b"invalid unmatched override",
            )],
            bsa::Compression::None,
        )
        .unwrap(),
    )
    .unwrap();
    fs::write(source.join("lod/stale.glb"), b"obsolete generation").unwrap();
    let report = rebuild(&data, &source, &output).await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    assert!(report.lod_chunks > 0);
    assert!(
        report
            .lod_warnings
            .iter()
            .any(|warning| warning.contains("no matching source-package plugin"))
    );
    assert!(!output.join("lod/stale.glb").exists());
    assert!(!output.join("vfs/misc/unrelated.txt").exists());
    let connection = Connection::open(output.join("skyrim_world.db")).unwrap();
    let origin: (i32, i32) = connection
        .query_row(
            "SELECT lod_origin_x,lod_origin_y FROM worldspaces WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(origin, (-4, -4));
    let indexed: u64 = connection
        .query_row("SELECT count(*) FROM lod_chunks_spatial", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(indexed, report.lod_chunks);
}

/// Package-matched selection must not admit a recovered-away longer owner's LOD settings.
#[tokio::test]
async fn metadata_rebuild_excludes_recovered_longer_archive_owner() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("derived");
    generate(&data);
    fs::remove_file(data.join("lodsettings/GeneratedWorld.lod")).unwrap();

    // This is a usable plugin header with a missing dependency, not undecodable bytes.
    let missing_master = b"Missing.esm\0";
    let header = [
        b"MAST".as_slice(),
        &(missing_master.len() as u16).to_le_bytes(),
        missing_master,
        b"DATA".as_slice(),
        &8u16.to_le_bytes(),
        &[0; 8],
    ]
    .concat();
    fs::write(
        data.join("Skyrim - ZBroken.esp"),
        [
            b"TES4".as_slice(),
            &(header.len() as u32).to_le_bytes(),
            &[0; 16],
            &header,
        ]
        .concat(),
    )
    .unwrap();
    let mut excluded_settings = PACKED_SETTINGS;
    excluded_settings[..2].copy_from_slice(&(-8i16).to_le_bytes());
    excluded_settings[2..4].copy_from_slice(&(-8i16).to_le_bytes());
    fs::write(
        data.join("Skyrim - ZBroken.bsa"),
        bsa::v105(
            &[Entry::new(
                "LODSettings/GeneratedWorld.LOD",
                &excluded_settings,
            )],
            bsa::Compression::None,
        )
        .unwrap(),
    )
    .unwrap();

    let converted = convert(&data, &source).await;
    assert!(
        converted
            .notices
            .iter()
            .any(|warning| warning.contains("Missing.esm"))
    );
    let source_manifest_hash = hash_file(&source.join("conversion-manifest.json")).unwrap();
    let source_database_hash = hash_file(&source.join("skyrim_world.db")).unwrap();
    let manifest = ConversionManifest::load(&source.join("conversion-manifest.json")).unwrap();
    {
        let connection = Connection::open(source.join("skyrim_world.db")).unwrap();
        converter::esm::exporter::validate_database(&connection).unwrap();
        let names: Vec<String> = connection
            .prepare("SELECT name FROM plugins ORDER BY priority")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(names, ["Skyrim.esm"]);
        let origin: (i32, i32) = connection
            .query_row(
                "SELECT lod_origin_x,lod_origin_y FROM worldspaces WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(origin, (-4, -4));
    }

    let report = rebuild(&data, &source, &output).await.unwrap();
    assert!(report.complete && report.lod_chunks > 0);
    assert_eq!(report.converted, 0);
    for entry in manifest.entries.values() {
        assert_eq!(
            hash_file(&output.join(&entry.output)).unwrap(),
            entry.output_hash
        );
        assert_eq!(
            hash_file(&source.join(&entry.output)).unwrap(),
            entry.output_hash
        );
    }
    let connection = Connection::open(output.join("skyrim_world.db")).unwrap();
    converter::esm::exporter::validate_database(&connection).unwrap();
    let version: u32 = connection
        .query_row("SELECT version FROM schema_info", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, shared::WORLD_DATABASE_SCHEMA_VERSION);
    let origin: (i32, i32) = connection
        .query_row(
            "SELECT lod_origin_x,lod_origin_y FROM worldspaces WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(origin, (-4, -4));
    drop(connection);
    assert_eq!(
        hash_file(&source.join("conversion-manifest.json")).unwrap(),
        source_manifest_hash
    );
    assert_eq!(
        hash_file(&source.join("skyrim_world.db")).unwrap(),
        source_database_hash
    );
}

/// Verified reuse preserves the repaired URI and bytes despite changed current Data assets.
#[tokio::test]
async fn metadata_rebuild_preserves_repaired_volcanic_normal_uri() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("derived");
    generate(&data);
    add_volcanic_source(&data);
    convert(&data, &source).await;
    let uri = volcanic_normal_uri(&source.join(VOLCANIC_GLB)).unwrap();
    assert!(uri.contains("textures/dlc02/landscape/grass/volcanicashgrass02_n"));
    let manifest = ConversionManifest::load(&source.join("conversion-manifest.json")).unwrap();
    assert!(
        manifest.entries[VOLCANIC_MODEL]
            .source_hash
            .ends_with(":volcanic-normal-repair:1:original=false:target=true")
    );
    let source_manifest_hash = hash_file(&source.join("conversion-manifest.json")).unwrap();
    let retained_hash = hash_file(&source.join(VOLCANIC_GLB)).unwrap();
    assert!(
        !source.join("vfs").exists(),
        "exercise normal runtime-package reuse"
    );

    // These changes would undo the repair during normal conversion. Reuse keeps SOURCE's bytes.
    let original = data.join(VOLCANIC_ORIGINAL);
    fs::create_dir_all(original.parent().unwrap()).unwrap();
    fs::copy(data.join(layout::GENERATED_NORMAL_PATH), original).unwrap();
    fs::remove_file(data.join(VOLCANIC_TARGET)).unwrap();
    fs::write(data.join(VOLCANIC_MODEL), b"changed current NIF override").unwrap();
    let report = rebuild(&data, &source, &output).await.unwrap();
    assert!(report.complete);
    assert_eq!(report.converted, 0);
    assert_eq!(volcanic_normal_uri(&output.join(VOLCANIC_GLB)), Some(uri));
    assert_eq!(
        hash_file(&output.join(VOLCANIC_GLB)).unwrap(),
        retained_hash
    );
    for entry in manifest.entries.values() {
        assert_eq!(
            hash_file(&output.join(&entry.output)).unwrap(),
            entry.output_hash
        );
    }
    let provenance: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("metadata-rebuild.json")).unwrap()).unwrap();
    assert_eq!(
        provenance["replay_verified_pruned_assets"],
        serde_json::json!([])
    );
    assert_eq!(
        hash_file(&source.join("conversion-manifest.json")).unwrap(),
        source_manifest_hash
    );
}

/// Legacy replay accepts old and scoped hashes using SOURCE's context and never applies repair.
#[tokio::test]
async fn metadata_rebuild_replays_volcanic_prunes_with_legacy_and_scoped_source_hashes() {
    for (scoped_hash, wrong_presence, corrupt_nif) in [
        (false, false, false),
        (true, false, false),
        (true, true, false),
        (false, false, true),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let source = directory.path().join("source");
        let output = directory.path().join("derived");
        let preprune = directory.path().join("preprune");
        generate(&data);
        add_volcanic_source(&data);
        let skeleton_key = "meshes/landscape/grass/skeleton.nif";
        fs::copy(
            data.join(layout::GENERATED_MODEL_PATH),
            data.join(skeleton_key),
        )
        .unwrap();
        convert(&data, &source).await;
        let historical_hash = format!(
            "{}:{}",
            hash_file(&data.join(VOLCANIC_MODEL)).unwrap(),
            hash_file(&data.join(skeleton_key)).unwrap()
        );
        let scoped_source_hash =
            format!("{historical_hash}:volcanic-normal-repair:1:original=false:target=true");
        let converted = ConversionManifest::load(&source.join("conversion-manifest.json")).unwrap();
        assert_eq!(
            converted.entries[VOLCANIC_MODEL].source_hash,
            scoped_source_hash
        );

        // Build the historical raw GLB and retain a pure prune with no DLC02 repair available.
        let candidate = preprune.join(VOLCANIC_GLB);
        converter::mesh::MeshConverter::convert_nif_to_glb(
            data.join(VOLCANIC_MODEL),
            candidate.clone(),
        )
        .unwrap();
        let original_hash = hash_file(&candidate).unwrap();
        let original_size = fs::metadata(&candidate).unwrap().len();
        for dependency in converter::mesh::MeshConverter::glb_texture_dependencies(&candidate)
            .unwrap()
            .into_iter()
            .filter(|dependency| !dependency.uri.contains("volcanicashgrass02_n"))
        {
            let published = converter::asset_path::resolve_asset_uri(
                &source,
                &source.join(VOLCANIC_GLB),
                &dependency.uri,
            )
            .unwrap();
            let retained =
                converter::asset_path::resolve_asset_uri(&preprune, &candidate, &dependency.uri)
                    .unwrap();
            fs::create_dir_all(retained.parent().unwrap()).unwrap();
            fs::copy(published, retained).unwrap();
        }
        let pruned =
            converter::mesh::MeshConverter::prune_dangling_texture_uris(&preprune).unwrap();
        assert_eq!(pruned.len(), 1);
        assert_eq!(pruned[0].removed_uris.len(), 1);
        let missing = converter::asset_path::resolve_asset_uri(
            &preprune,
            &candidate,
            &pruned[0].removed_uris[0],
        )
        .unwrap();
        let missing = missing
            .strip_prefix(&preprune)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        assert_eq!(
            missing,
            "textures/landscape/grass/volcanicashgrass02_n.opensky-wrap0.ktx2"
        );
        assert!(volcanic_normal_uri(&candidate).is_none());
        fs::copy(&candidate, source.join(VOLCANIC_GLB)).unwrap();

        let retained_nif = source.join("vfs").join(VOLCANIC_MODEL);
        fs::create_dir_all(retained_nif.parent().unwrap()).unwrap();
        fs::copy(data.join(VOLCANIC_MODEL), &retained_nif).unwrap();
        fs::copy(
            data.join(skeleton_key),
            source.join("vfs").join(skeleton_key),
        )
        .unwrap();
        let mut manifest =
            ConversionManifest::load(&source.join("conversion-manifest.json")).unwrap();
        let entry = manifest.entries.get_mut(VOLCANIC_MODEL).unwrap();
        entry.output_hash = original_hash;
        entry.output_size = original_size;
        // Both historical contracts hash the discovered skeleton before the scoped suffix.
        entry.source_hash = historical_hash;
        if scoped_hash {
            entry.source_hash.push_str(if wrong_presence {
                ":volcanic-normal-repair:1:original=true:target=true"
            } else {
                ":volcanic-normal-repair:1:original=false:target=true"
            });
        }
        manifest
            .pruned_texture_references
            .insert(VOLCANIC_GLB.into(), [missing].into());
        manifest
            .save(&source.join("conversion-manifest.json"))
            .unwrap();
        let source_manifest_hash = hash_file(&source.join("conversion-manifest.json")).unwrap();
        let retained_hash = hash_file(&source.join(VOLCANIC_GLB)).unwrap();
        if corrupt_nif {
            fs::write(&retained_nif, b"tampered retained source NIF").unwrap();
        }

        // Data's current presence bits differ; only the retained package can verify its hash.
        let original = data.join(VOLCANIC_ORIGINAL);
        fs::create_dir_all(original.parent().unwrap()).unwrap();
        fs::copy(data.join(layout::GENERATED_NORMAL_PATH), original).unwrap();
        fs::remove_file(data.join(VOLCANIC_TARGET)).unwrap();
        let result = rebuild(&data, &source, &output).await;
        if wrong_presence || corrupt_nif {
            let error = result.unwrap_err().to_string();
            assert!(
                error.contains("pruned GLB source checksum mismatch"),
                "{error}"
            );
            assert!(!output.exists());
        } else {
            let report = result.unwrap();
            assert!(report.complete);
            assert_eq!(
                hash_file(&output.join(VOLCANIC_GLB)).unwrap(),
                retained_hash
            );
            assert!(volcanic_normal_uri(&output.join(VOLCANIC_GLB)).is_none());
            let updated =
                ConversionManifest::load(&output.join("conversion-manifest.json")).unwrap();
            assert_eq!(updated.entries[VOLCANIC_MODEL].output_hash, retained_hash);
            let provenance: serde_json::Value =
                serde_json::from_slice(&fs::read(output.join("metadata-rebuild.json")).unwrap())
                    .unwrap();
            assert_eq!(
                provenance["replay_verified_pruned_assets"],
                serde_json::json!([VOLCANIC_GLB])
            );
        }
        assert_eq!(
            hash_file(&source.join(VOLCANIC_GLB)).unwrap(),
            retained_hash
        );
        assert_eq!(
            hash_file(&source.join("conversion-manifest.json")).unwrap(),
            source_manifest_hash
        );
    }
}

#[tokio::test]
async fn metadata_rebuild_v87_accepts_old_prune_hashes_only_after_exact_source_replay() {
    for (corrupt, with_skeleton, with_vfs) in [
        (false, false, true),
        (true, false, true),
        (false, true, true),
        (false, false, false),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let source = directory.path().join("source");
        let output = directory.path().join("derived");
        generate(&data);
        if with_skeleton {
            // Static meshes also record a discovered skeleton's dependency hash.
            fs::copy(
                data.join(layout::GENERATED_MODEL_PATH),
                data.join("meshes/skeleton.nif"),
            )
            .unwrap();
        }
        fs::remove_file(data.join(layout::GENERATED_NORMAL_PATH)).unwrap();
        convert(&data, &source).await;
        // Historical packs kept verified raw VFS files; main's runtime packs do not.
        let legacy_meshes = source.join("vfs/meshes");
        fs::create_dir_all(&legacy_meshes).unwrap();
        fs::copy(
            data.join(layout::GENERATED_MODEL_PATH),
            legacy_meshes.join("generated.nif"),
        )
        .unwrap();
        if with_skeleton {
            fs::copy(
                data.join("meshes/skeleton.nif"),
                legacy_meshes.join("skeleton.nif"),
            )
            .unwrap();
        }
        let mut manifest =
            ConversionManifest::load(&source.join("conversion-manifest.json")).unwrap();
        let key = layout::GENERATED_MODEL_PATH;
        let candidate = directory.path().join("preprune/meshes/generated.glb");
        converter::mesh::MeshConverter::convert_nif_to_glb(data.join(key), candidate.clone())
            .unwrap();
        let entry = manifest.entries.get_mut(key).unwrap();
        entry.output_hash = hash_file(&candidate).unwrap();
        entry.output_size = fs::metadata(&candidate).unwrap().len();
        if !corrupt && !with_skeleton && with_vfs {
            let mut historical = manifest.clone();
            historical.entries.get_mut(key).unwrap().output_size += 1;
            historical
                .save(&source.join("conversion-manifest.json"))
                .unwrap();
            let error = rebuild(&data, &source, &output)
                .await
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("cannot reproduce original pruned GLB checksum"),
                "{error}"
            );
            assert!(
                error.contains("Run a normal conversion into a new package"),
                "{error}"
            );
            assert!(!output.exists());
        }
        manifest
            .save(&source.join("conversion-manifest.json"))
            .unwrap();
        if corrupt {
            fs::write(source.join("meshes/generated.glb"), b"tampered").unwrap();
        }
        if !with_vfs {
            fs::remove_dir_all(source.join("vfs")).unwrap();
        }
        let retained = hash_file(&source.join("meshes/generated.glb")).unwrap();
        let result = rebuild(&data, &source, &output).await;
        if !with_vfs {
            let error = result.unwrap_err().to_string();
            assert!(
                error.contains("legacy pruned GLB replay requires raw source NIF"),
                "{error}"
            );
            assert!(!output.exists());
        } else if corrupt {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("checksum mismatch")
            );
            assert!(!output.exists());
        } else {
            assert!(result.unwrap().complete);
            assert_eq!(
                hash_file(&output.join("meshes/generated.glb")).unwrap(),
                retained
            );
            let updated =
                ConversionManifest::load(&output.join("conversion-manifest.json")).unwrap();
            assert_eq!(updated.entries[key].output_hash, retained);
            let provenance: serde_json::Value =
                serde_json::from_slice(&fs::read(output.join("metadata-rebuild.json")).unwrap())
                    .unwrap();
            assert_eq!(
                provenance["replay_verified_pruned_assets"],
                serde_json::json!(["meshes/generated.glb"])
            );
        }
        assert_eq!(
            hash_file(&source.join("meshes/generated.glb")).unwrap(),
            retained
        );
    }
}

/// Historical producer 17/18 and the separate 25 candidates are ambiguous for rebuild.
#[tokio::test]
async fn v91_metadata_rebuild_rejects_ambiguous_lod_and_lighting_producers() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    generate(&data);
    convert(&data, &source).await;
    let path = source.join("conversion-manifest.json");
    let mut manifest: ConversionManifest =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for schema in [17, 18, 25] {
        manifest.schema_version = schema;
        if schema != converter::cache::CONVERTER_SCHEMA_VERSION {
            manifest.asset_configuration_hashes.clear();
        }
        manifest.save(&path).unwrap();
        let output = directory.path().join(format!("derived-{schema}"));
        let error = rebuild(&data, &source, &output)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("predate the current texture layout"),
            "{error}"
        );
        assert!(!output.exists());
    }
}

/// The settings proof a producer with script ABI `abi` and reader `identity` recorded.
/// The fixture's default configuration uses the CPU texture encoder.
fn combined_hash(config: &PipelineConfig, identity: &serde_json::Value, abi: u32) -> String {
    assert_eq!(config.texture_encoder, converter::TextureEncoder::Cpu);
    hash_bytes(
        &serde_json::to_vec(&serde_json::json!({
            "schema": converter::cache::CONVERTER_SCHEMA_VERSION,
            "texture_etc1s_quality": config.texture_fallback_quality,
            "texture_uastc_level": config.texture_uastc_level,
            "script_abi_version": abi,
            "record_reader": identity,
            "texture_zstd_level": config.texture_zstd_level,
            "texture_encoder": {"mode": "cpu"},
        }))
        .unwrap(),
    )
}

/// Replace a file without writing through a hard link.
fn replace_file(path: &Path, bytes: &[u8]) {
    fs::remove_file(path).unwrap();
    fs::write(path, bytes).unwrap();
}

/// Make `source` look like a pack produced under script ABI `abi`, and optionally decoded
/// with an earlier base schema. Its scripts become marker bytes that must never survive a
/// rebuild. Returns the scripts a conversion under the current ABI produced.
fn age_source(
    source: &Path,
    data: &Path,
    abi: u32,
    earlier_schema: Option<&str>,
) -> std::collections::BTreeMap<String, Vec<u8>> {
    let config = common::cpu_lod_config(data, source.join("unused"));
    let path = source.join("conversion-manifest.json");
    let mut manifest = ConversionManifest::load(&path).unwrap();
    let mut identity: serde_json::Value =
        serde_json::from_slice(&fs::read(source.join("record-reader.json")).unwrap()).unwrap();
    assert_eq!(
        combined_hash(&config, &identity, converter::script::SCRIPT_ABI_VERSION),
        manifest.configuration_hash,
        "the test must reproduce the converter's combined proof"
    );
    if let Some(digest) = earlier_schema {
        identity["schema_sha256"] = serde_json::json!(digest);
        replace_file(
            &source.join("record-reader.json"),
            &serde_json::to_vec(&identity).unwrap(),
        );
        Connection::open(source.join("skyrim_world.db"))
            .unwrap()
            .execute(
                "UPDATE record_overrides SET schema_sha256=?1 WHERE record_type='TES4'",
                [digest],
            )
            .unwrap();
    }
    manifest.configuration_hash = combined_hash(&config, &identity, abi);
    let mut old_abi = config.clone();
    old_abi.script_abi_version = abi;
    manifest.asset_configuration_hashes.insert(
        "pex".into(),
        converter::cache::asset_configuration_hash_for_schema(
            &old_abi,
            converter::cache::CONVERTER_SCHEMA_VERSION,
            "pex",
        )
        .unwrap(),
    );
    let marker = format!("-- script ABI {abi} marker\n");
    let mut originals = std::collections::BTreeMap::new();
    for entry in manifest
        .entries
        .values_mut()
        .filter(|entry| entry.output.starts_with("scripts/"))
    {
        let script = source.join(&entry.output);
        originals.insert(entry.output.clone(), fs::read(&script).unwrap());
        replace_file(&script, marker.as_bytes());
        entry.output_hash = hash_bytes(marker.as_bytes());
        entry.output_size = marker.len() as u64;
    }
    assert!(!originals.is_empty(), "the fixture has scripts");
    replace_file(
        &source.join("scripts/papyrus_runtime.luau"),
        b"-- runtime for an older script ABI\n",
    );
    manifest.save(&path).unwrap();
    originals
}

fn provenance(output: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(output.join("metadata-rebuild.json")).unwrap()).unwrap()
}

/// A script ABI bump rebuilds only the scripts. Meshes, textures and every neighbor stay the
/// source's verified bytes, the source pack is not touched, and the next rebuild from the
/// result reuses every kind.
#[tokio::test]
async fn script_abi_bump_rebuilds_only_scripts_and_reuses_the_rest() {
    check_script_abi_upgrade(1).await;
    check_script_abi_upgrade(3).await;
}

async fn check_script_abi_upgrade(source_abi: u32) {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("rebuilt");
    let again = directory.path().join("again");
    generate(&data);
    convert(&data, &source).await;
    let originals = age_source(&source, &data, source_abi, None);
    let aged_manifest = hash_file(&source.join("conversion-manifest.json")).unwrap();
    let manifest = ConversionManifest::load(&source.join("conversion-manifest.json")).unwrap();
    let kept: Vec<_> = manifest
        .entries
        .values()
        .filter(|entry| !entry.output.starts_with("scripts/"))
        .map(|entry| {
            (
                entry.output.clone(),
                hash_file(&source.join(&entry.output)).unwrap(),
            )
        })
        .collect();
    assert!(
        kept.iter().any(|(path, _)| path.ends_with(".glb"))
            && kept.iter().any(|(path, _)| path.ends_with(".ktx2"))
    );

    let report = rebuild(&data, &source, &output).await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    assert_eq!(
        report.converted,
        originals.len() as u64,
        "only scripts convert"
    );
    for (script, bytes) in &originals {
        assert_eq!(&fs::read(output.join(script)).unwrap(), bytes, "{script}");
    }
    assert_eq!(
        fs::read(output.join("scripts/papyrus_runtime.luau")).unwrap(),
        include_bytes!("../../shared/src/papyrus_runtime.luau")
    );
    for (relative, hash) in &kept {
        assert_eq!(
            &hash_file(&output.join(relative)).unwrap(),
            hash,
            "{relative}"
        );
    }
    let rebuilt = ConversionManifest::load(&output.join("conversion-manifest.json")).unwrap();
    let current_pex = converter::cache::asset_configuration_hash_for_schema(
        &common::cpu_lod_config(&data, &output),
        converter::cache::CONVERTER_SCHEMA_VERSION,
        "pex",
    )
    .unwrap();
    assert_eq!(rebuilt.asset_configuration_hashes["pex"], current_pex);
    for (script, bytes) in &originals {
        let entry = rebuilt
            .entries
            .values()
            .find(|entry| &entry.output == script)
            .unwrap();
        assert_eq!(entry.output_hash, hash_bytes(bytes));
    }
    let record = provenance(&output);
    assert_eq!(record["source_script_abi"], source_abi);
    assert_eq!(record["script_abi"], converter::script::SCRIPT_ABI_VERSION);
    assert_eq!(record["asset_kinds"]["pex"]["action"], "rebuilt");
    assert_eq!(record["asset_kinds"]["nif"]["action"], "reused");
    assert_eq!(record["asset_kinds"]["dds"]["action"], "reused");
    assert_eq!(
        record["asset_kinds"]["pex"]["proof"].as_str().unwrap(),
        current_pex
    );
    // The source stays exactly as it was, markers included.
    assert_eq!(
        hash_file(&source.join("conversion-manifest.json")).unwrap(),
        aged_manifest
    );
    for script in originals.keys() {
        assert_eq!(
            fs::read(source.join(script)).unwrap(),
            format!("-- script ABI {source_abi} marker\n").as_bytes()
        );
    }

    // The result is a current pack: the next rebuild reuses every kind, scripts included.
    let report = rebuild(&data, &output, &again).await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    assert_eq!(report.converted, 0);
    let record = provenance(&again);
    for kind in ["nif", "dds", "pex"] {
        assert_eq!(record["asset_kinds"][kind]["action"], "reused", "{kind}");
    }
    for (script, bytes) in &originals {
        assert_eq!(&fs::read(again.join(script)).unwrap(), bytes);
    }
}

/// A recorded earlier base schema in the pack's lineage no longer refuses the rebuild; the
/// database is decoded again with the compiled schema. An unlisted digest still refuses.
#[tokio::test]
async fn trusted_earlier_schema_digest_is_accepted_and_unknown_digest_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("rebuilt");
    let again = directory.path().join("again");
    generate(&data);
    convert(&data, &source).await;
    let earlier = "ee7e85d996711c656abcc427fb76496452c7d0e89c1b383abf64a8acd1392d30";
    // The ABI changed too, as in the registered packs.
    let originals = age_source(&source, &data, 1, Some(earlier));

    let report = rebuild(&data, &source, &output).await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    assert_eq!(report.converted, originals.len() as u64);
    let record = provenance(&output);
    assert_eq!(record["source_schema_lineage"]["sha256"], earlier);
    assert_eq!(
        record["source_schema_lineage"]["produced_by"],
        "b5259d33a73e"
    );
    let reader: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("record-reader.json")).unwrap()).unwrap();
    assert_eq!(
        reader["schema_sha256"],
        hash_bytes(converter::records::SCHEMA_BYTES)
    );
    // The retained producer keeps its earlier schema, so a second rebuild still accepts it.
    assert_eq!(
        record["retained_asset_record_reader"]["schema_sha256"],
        earlier
    );
    let report = rebuild(&data, &output, &again).await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    assert_eq!(report.converted, 0);

    let unknown = directory.path().join("unknown-source");
    convert(&data, &unknown).await;
    age_source(&unknown, &data, 1, Some(&"ab".repeat(32)));
    let refused = directory.path().join("unknown-output");
    let error = rebuild(&data, &unknown, &refused).await.unwrap_err();
    assert!(
        format!("{error:?}").contains("diagnostic schema differs from the producing schema"),
        "{error:?}"
    );
    assert!(!refused.exists());
}

/// A real asset change is never reused or relabeled: a stale mesh proof refuses the rebuild
/// even when the scripts would also rebuild, and names the kind to regenerate.
#[tokio::test]
async fn changed_mesh_proof_still_refuses_alongside_a_script_abi_bump() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("rebuilt");
    generate(&data);
    convert(&data, &source).await;
    age_source(&source, &data, 1, None);
    let path = source.join("conversion-manifest.json");
    let mut manifest = ConversionManifest::load(&path).unwrap();
    manifest
        .asset_configuration_hashes
        .insert("nif".into(), "0".repeat(64));
    manifest.save(&path).unwrap();
    let error = rebuild(&data, &source, &output).await.unwrap_err();
    assert!(
        format!("{error:?}").contains("mesh output contract"),
        "{error:?}"
    );
    assert!(!output.exists());
}

/// Rebuilt scripts must come from the sources the package recorded.
#[tokio::test]
async fn script_rebuild_refuses_data_scripts_that_differ_from_the_package() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("rebuilt");
    generate(&data);
    convert(&data, &source).await;
    age_source(&source, &data, 1, None);
    replace_file(
        &data.join("scripts/generated.pex"),
        &dummy_content::pex::minimal("Changed").unwrap(),
    );
    let error = rebuild(&data, &source, &output).await.unwrap_err();
    assert!(
        format!("{error:?}").contains("script sources in Data differ"),
        "{error:?}"
    );
    assert!(!output.exists());
}

/// Every mesh or texture refusal names the safe path, its settings, its approval and its cost.
fn assert_regeneration_note(error: &str) {
    for needle in [
        "into an APFS clone of this package",
        "never into a registered pack",
        "--texture-encoder gpu --gpu-quality 2",
        "explicit approval",
        "65 GiB",
    ] {
        assert!(error.contains(needle), "missing {needle:?}: {error}");
    }
}

/// Copy a pack tree for the Unix inode-preservation fixture.
#[cfg(unix)]
fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A stale mesh proof is regenerated by a normal conversion into a clone of the pack. That
/// conversion converts only the meshes: every texture and script whose own proof still matches
/// stays the same file, and the source pack is not touched. The metadata rebuild refuses the
/// same pack and names this path.
#[cfg(unix)]
#[tokio::test]
async fn normal_conversion_into_a_clone_rebuilds_only_the_stale_mesh_kind() {
    use std::os::unix::fs::MetadataExt;
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let clone = directory.path().join("clone");
    generate(&data);
    convert(&data, &source).await;
    copy_tree(&source, &clone);
    let path = clone.join("conversion-manifest.json");
    let mut manifest = ConversionManifest::load(&path).unwrap();
    manifest
        .asset_configuration_hashes
        .insert("nif".into(), "0".repeat(64));
    manifest.save(&path).unwrap();
    let by_kind = |manifest: &ConversionManifest, prefix: &str| -> Vec<String> {
        manifest
            .entries
            .values()
            .filter(|entry| entry.output.starts_with(prefix))
            .map(|entry| entry.output.clone())
            .collect()
    };
    let (meshes, textures, scripts) = (
        by_kind(&manifest, "meshes/"),
        by_kind(&manifest, "textures/"),
        by_kind(&manifest, "scripts/"),
    );
    assert!(!meshes.is_empty() && !textures.is_empty() && !scripts.is_empty());
    let inodes = |root: &Path, files: &[String]| -> Vec<u64> {
        files
            .iter()
            .map(|file| fs::metadata(root.join(file)).unwrap().ino())
            .collect()
    };
    let before = (
        inodes(&clone, &meshes),
        inodes(&clone, &textures),
        inodes(&clone, &scripts),
    );
    let source_manifest = hash_file(&source.join("conversion-manifest.json")).unwrap();

    let refused = rebuild(&data, &clone, &directory.path().join("refused"))
        .await
        .unwrap_err();
    assert_regeneration_note(&format!("{refused:?}"));

    convert(&data, &clone).await;
    let current = converter::cache::asset_configuration_hash_for_schema(
        &common::cpu_lod_config(&data, &clone),
        converter::cache::CONVERTER_SCHEMA_VERSION,
        "nif",
    )
    .unwrap();
    let rebuilt = ConversionManifest::load(&path).unwrap();
    assert_eq!(rebuilt.asset_configuration_hashes["nif"], current);
    assert_eq!(
        (inodes(&clone, &textures), inodes(&clone, &scripts)),
        (before.1, before.2),
        "textures and scripts must stay the clone's files"
    );
    assert!(
        inodes(&clone, &meshes)
            .iter()
            .zip(&before.0)
            .all(|(now, then)| now != then),
        "every mesh must be converted again"
    );
    assert_eq!(
        hash_file(&source.join("conversion-manifest.json")).unwrap(),
        source_manifest
    );
}

/// Rewrite one manifest field of a pack in place.
fn edit_manifest(source: &Path, edit: impl FnOnce(&mut ConversionManifest)) {
    let path = source.join("conversion-manifest.json");
    let mut manifest = ConversionManifest::load(&path).unwrap();
    edit(&mut manifest);
    manifest.save(&path).unwrap();
}

/// A texture proof that differs refuses on its own, even when the combined proof matches and
/// the scripts would rebuild. The refusal names the clone-and-convert path.
#[tokio::test]
async fn changed_texture_proof_alone_refuses_a_rebuild() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("rebuilt");
    generate(&data);
    convert(&data, &source).await;
    age_source(&source, &data, 1, None);
    edit_manifest(&source, |manifest| {
        manifest
            .asset_configuration_hashes
            .insert("dds".into(), "0".repeat(64));
    });
    let error = format!("{:?}", rebuild(&data, &source, &output).await.unwrap_err());
    assert!(error.contains("texture settings"), "{error}");
    assert_regeneration_note(&error);
    assert!(!output.exists());
}

/// A combined proof that differs in any setting besides the script ABI finds no ABI and
/// refuses, whatever the per-kind proofs say.
#[tokio::test]
async fn combined_proof_differing_in_a_non_abi_setting_refuses() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("rebuilt");
    generate(&data);
    convert(&data, &source).await;
    age_source(&source, &data, 1, None);
    let config = common::cpu_lod_config(&data, &output);
    let identity: serde_json::Value =
        serde_json::from_slice(&fs::read(source.join("record-reader.json")).unwrap()).unwrap();
    let mut other = config.clone();
    other.texture_zstd_level += 1;
    edit_manifest(&source, |manifest| {
        manifest.configuration_hash = combined_hash(&other, &identity, 1);
    });
    let error = format!("{:?}", rebuild(&data, &source, &output).await.unwrap_err());
    assert!(
        error.contains("retained asset configuration does not match rebuild settings"),
        "{error}"
    );
    assert!(!output.exists());
}

/// The lineage path never skips the mesh check: a trusted earlier schema together with a stale
/// mesh proof still refuses.
#[tokio::test]
async fn trusted_earlier_schema_does_not_skip_the_mesh_check() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("rebuilt");
    generate(&data);
    convert(&data, &source).await;
    let earlier = "ee7e85d996711c656abcc427fb76496452c7d0e89c1b383abf64a8acd1392d30";
    age_source(&source, &data, 1, Some(earlier));
    edit_manifest(&source, |manifest| {
        manifest
            .asset_configuration_hashes
            .insert("nif".into(), "0".repeat(64));
    });
    let error = format!("{:?}", rebuild(&data, &source, &output).await.unwrap_err());
    assert!(error.contains("mesh output contract"), "{error}");
    assert!(!output.exists());
}

/// A saved configuration cannot name a script ABI newer than the converter emits.
#[tokio::test]
async fn saved_script_abi_newer_than_the_converter_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("rebuilt");
    generate(&data);
    convert(&data, &source).await;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let mut config = common::cpu_lod_config(&data, &output);
    config.script_abi_version = converter::script::SCRIPT_ABI_VERSION + 1;
    let error = AssetPipeline::rebuild_metadata_async(config, &source, tx)
        .await
        .unwrap_err();
    drain.await.unwrap();
    assert!(
        format!("{error:?}").contains("newer than the ABI"),
        "{error:?}"
    );
    assert!(!output.exists());
}

/// An output folder inside any asset pack is refused before a lock file or staging folder can
/// be created there.
#[tokio::test]
async fn output_inside_another_pack_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let other = directory.path().join("other");
    generate(&data);
    convert(&data, &source).await;
    convert(&data, &other).await;
    let output = other.join("rebuilt");
    let error = format!("{:?}", rebuild(&data, &source, &output).await.unwrap_err());
    assert!(error.contains("lies inside the asset pack"), "{error}");
    assert!(!output.exists());
    let leftovers: Vec<_> = fs::read_dir(&other)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().contains("rebuilt"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

/// Metadata-only upgrades add controls from current providers and retain every verified asset byte.
#[tokio::test]
async fn control_files_metadata_rebuild_upgrades_an_older_pack() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("rebuilt");
    generate(&data);
    convert(&data, &source).await;
    assert!(!source.join("interface").exists());
    let manifest_path = source.join("conversion-manifest.json");
    let source_hash = hash_file(&manifest_path).unwrap();
    let manifest = ConversionManifest::load(&manifest_path).unwrap();
    let control = b"Synth Action\t0x17\t0xff\t0x2000\t1\t0\t1\r\nbad line\r\nSynth Other\t0x18\t0xff\t0x1000\t1\t0\t1\r\n";
    fs::write(
        data.join("Skyrim - Interface.bsa"),
        bsa::v105(
            &[
                Entry::new("Interface/Controls/PC/ControlMap.txt", control),
                Entry::new(
                    "interface/controls/pc/keyboard_english.txt",
                    b"Synth\t0x17\t23\r\n",
                ),
                Entry::new("interface/controls/pc/gamepad.txt", b"SYNTH_A\t0x1000\r\n"),
            ],
            bsa::Compression::None,
        )
        .unwrap(),
    )
    .unwrap();
    let loose = data.join("Interface/Controls/PC/Keyboard_English.TXT");
    fs::create_dir_all(loose.parent().unwrap()).unwrap();
    fs::write(&loose, b"Modded\t0x33\r\n").unwrap();
    let report = rebuild(&data, &source, &output).await.unwrap();
    assert!(
        output
            .join("interface/controls/pc/controlmap.txt")
            .is_file()
    );
    assert_eq!(
        fs::read(output.join("interface/controls/pc/controlmap.txt")).unwrap(),
        [
            b"Synth Action\t0x17\t0xff\t0x2000\t1\t0\t1\r\n".as_slice(),
            b"Synth Other\t0x18\t0xff\t0x1000\t1\t0\t1\r\n".as_slice(),
        ]
        .concat()
    );
    assert_eq!(
        fs::read(output.join("interface/controls/pc/keyboard_english.txt")).unwrap(),
        b"Modded\t0x33\r\n"
    );
    assert!(!report.complete);
    assert_eq!(
        report
            .warnings
            .iter()
            .filter(|warning| warning.contains("controlmap.txt:2"))
            .count(),
        1
    );
    assert_eq!(hash_file(&manifest_path).unwrap(), source_hash);
    for entry in manifest.entries.values() {
        assert_eq!(
            fs::read(output.join(&entry.output)).unwrap(),
            fs::read(source.join(&entry.output)).unwrap(),
            "{}",
            entry.output
        );
    }
}

/// Snapshot every package file so rebuild recovery proves its source remained immutable.
fn script_recovery_package_hashes(root: &Path) -> std::collections::BTreeMap<String, String> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .map(Result::unwrap)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| {
            (
                entry
                    .path()
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                hash_file(entry.path()).unwrap(),
            )
        })
        .collect()
}

/// Verify incomplete publication retains world, grass, models, textures and healthy scripts.
fn assert_script_recovery_publication(
    source: &Path,
    output: &Path,
    report: &converter::PipelineReport,
    originals: &std::collections::BTreeMap<String, Vec<u8>>,
    omitted: &str,
) {
    assert!(
        !report.complete,
        "a skipped script must mark publication incomplete"
    );
    assert!(
        !report.warnings.is_empty(),
        "script loss needs a report warning"
    );
    assert!(report.skipped > 0);
    let manifest = ConversionManifest::load(&output.join("conversion-manifest.json")).unwrap();
    assert!(!manifest.complete);
    assert!(
        !manifest.failures.is_empty(),
        "script loss needs a manifest failure"
    );
    assert!(!manifest.entries.contains_key(omitted));
    let bad_output = omitted.replace(".pex", ".luau");
    assert!(
        !output.join(&bad_output).exists(),
        "old ABI bytes must be omitted"
    );
    for (name, bytes) in originals {
        if name != &bad_output {
            assert_eq!(
                &fs::read(output.join(name)).unwrap(),
                bytes,
                "healthy script {name}"
            );
        }
    }
    let source_manifest =
        ConversionManifest::load(&source.join("conversion-manifest.json")).unwrap();
    for entry in source_manifest
        .entries
        .values()
        .filter(|entry| entry.output.ends_with(".glb") || entry.output.ends_with(".ktx2"))
    {
        assert_eq!(
            hash_file(&output.join(&entry.output)).unwrap(),
            entry.output_hash
        );
    }
    assert!(output.join("grass_cache.rkyv").is_file());
    assert_eq!(
        fs::read(output.join("scripts/papyrus_runtime.luau")).unwrap(),
        include_bytes!("../../shared/src/papyrus_runtime.luau")
    );
    let connection = Connection::open(output.join("skyrim_world.db")).unwrap();
    let count: u64 = connection
        .query_row(
            "SELECT COUNT(*) FROM \"references\" WHERE id=4660",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "healthy neighboring world record must publish");
}

/// A recorded unchanged script rejected by the current decoder omits only that script.
#[tokio::test]
async fn metadata_script_recovery_skips_recorded_bad_pex_and_publishes_neighbors() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("rebuilt");
    generate(&data);
    fs::write(
        data.join("scripts/bad.pex"),
        dummy_content::pex::minimal("Bad").unwrap(),
    )
    .unwrap();
    convert(&data, &source).await;
    let originals = age_source(&source, &data, 1, None);
    let broken = b"unchanged historical PEX rejected by the current decoder";
    replace_file(&data.join("scripts/bad.pex"), broken);
    edit_manifest(&source, |manifest| {
        manifest
            .entries
            .get_mut("scripts/bad.pex")
            .unwrap()
            .source_hash = hash_bytes(broken);
    });
    let before = script_recovery_package_hashes(&source);
    let result = rebuild(&data, &source, &output).await;
    assert!(
        result.is_ok(),
        "one unchanged bad PEX must preserve publication: {result:?}"
    );
    let report = result.unwrap();
    assert_script_recovery_publication(&source, &output, &report, &originals, "scripts/bad.pex");
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("bad.pex")),
        "{:?}",
        report.warnings
    );
    assert_eq!(script_recovery_package_hashes(&source), before);
}

/// A valid archive table can identify a selected script whose compressed payload is corrupt.
#[tokio::test]
async fn metadata_script_recovery_skips_corrupt_selected_archive_and_publishes_neighbors() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("rebuilt");
    generate(&data);
    let bad_pex = dummy_content::pex::minimal("ArchivedBad").unwrap();
    let good_pex = dummy_content::pex::minimal("ArchivedGood").unwrap();
    let same_archive_pex = dummy_content::pex::minimal("SameArchiveGood").unwrap();
    let archive = data.join("Skyrim - Scripts.bsa");
    let mut bytes = bsa::v104(
        &[
            Entry {
                name: "scripts/samearchivegood.pex",
                data: &same_archive_pex,
            },
            Entry {
                name: "scripts/archivebad.pex",
                data: &bad_pex,
            },
        ],
        bsa::Compression::Zlib,
    )
    .unwrap();
    fs::write(&archive, &bytes).unwrap();
    fs::write(
        data.join("Skyrim - Neighbor.bsa"),
        bsa::v105(
            &[Entry {
                name: "scripts/archivegood.pex",
                data: &good_pex,
            }],
            bsa::Compression::None,
        )
        .unwrap(),
    )
    .unwrap();
    convert(&data, &source).await;
    let originals = age_source(&source, &data, 1, None);
    assert!(originals.contains_key("scripts/archivebad.luau"));
    assert!(originals.contains_key("scripts/archivegood.luau"));
    assert!(originals.contains_key("scripts/samearchivegood.luau"));
    let record = 36 + 16 + 1 + "scripts".len() + 1 + 16;
    let payload = u32::from_le_bytes(bytes[record + 12..record + 16].try_into().unwrap()) as usize;
    assert!(payload + 8 < bytes.len());
    bytes[payload + 4..].fill(0xff);
    replace_file(&archive, &bytes);
    let before = script_recovery_package_hashes(&source);
    let result = rebuild(&data, &source, &output).await;
    assert!(
        result.is_ok(),
        "one corrupt selected archive must preserve publication: {result:?}"
    );
    let report = result.unwrap();
    assert_script_recovery_publication(
        &source,
        &output,
        &report,
        &originals,
        "scripts/archivebad.pex",
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("Skyrim - Scripts.bsa")),
        "{:?}",
        report.warnings
    );
    assert_eq!(script_recovery_package_hashes(&source), before);
    assert_eq!(fs::read(&archive).unwrap(), bytes);
}

/// Source authentication precedes decoding so changed invalid bytes remain a package refusal.
#[tokio::test]
async fn metadata_script_recovery_refuses_changed_invalid_pex_identity() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("rebuilt");
    generate(&data);
    convert(&data, &source).await;
    age_source(&source, &data, 1, None);
    replace_file(&data.join("scripts/generated.pex"), b"changed invalid PEX");
    let before = script_recovery_package_hashes(&source);
    let error = rebuild(&data, &source, &output).await.unwrap_err();
    assert!(
        format!("{error:?}").contains("script sources in Data differ"),
        "{error:?}"
    );
    assert!(!output.exists());
    assert_eq!(script_recovery_package_hashes(&source), before);
}

/// Extra invalid scripts are authenticated as changed input even when the decoder rejects them.
#[tokio::test]
async fn metadata_script_recovery_refuses_extra_invalid_pex_identity() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("rebuilt");
    generate(&data);
    convert(&data, &source).await;
    age_source(&source, &data, 1, None);
    fs::write(data.join("scripts/extra.pex"), b"extra invalid PEX").unwrap();
    let before = script_recovery_package_hashes(&source);
    let error = rebuild(&data, &source, &output).await.unwrap_err();
    assert!(
        format!("{error:?}").contains("script sources in Data differ"),
        "{error:?}"
    );
    assert!(!output.exists());
    assert_eq!(script_recovery_package_hashes(&source), before);
}

/// Explicit fail-fast still returns the script error before publishing the rebuilt directory.
#[tokio::test]
async fn metadata_script_recovery_respects_fail_fast() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("rebuilt");
    generate(&data);
    convert(&data, &source).await;
    age_source(&source, &data, 1, None);
    let broken = b"recorded invalid PEX for explicit fail-fast";
    replace_file(&data.join("scripts/generated.pex"), broken);
    edit_manifest(&source, |manifest| {
        manifest
            .entries
            .get_mut("scripts/generated.pex")
            .unwrap()
            .source_hash = hash_bytes(broken);
    });
    let before = script_recovery_package_hashes(&source);
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let mut config = common::cpu_lod_config(&data, &output);
    config.fail_fast = true;
    let result = AssetPipeline::rebuild_metadata_async(config, &source, tx).await;
    drain.await.unwrap();
    assert!(result.is_err(), "explicit fail-fast must stop publication");
    assert!(!output.exists());
    assert_eq!(script_recovery_package_hashes(&source), before);
}

/// Publish verified neighboring scripts and world/assets while dropping unusable archive names.
async fn metadata_odd_archive_names_publication(corrupt_payload: bool, fail_fast: bool) {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("rebuilt");
    generate(&data);
    let good = dummy_content::pex::minimal("OddNameNeighbor").unwrap();
    let bad = dummy_content::pex::minimal("OddNameRecorded").unwrap();
    let odd = dummy_content::pex::minimal("UnidentifiableExtra").unwrap();
    let archive = data.join("Skyrim - Scripts.bsa");
    let baseline = bsa::v104(
        &[
            Entry {
                name: "scripts/samearchivegood.pex",
                data: &good,
            },
            Entry {
                name: "scripts/archivebad.pex",
                data: &bad,
            },
        ],
        bsa::Compression::Zlib,
    )
    .unwrap();
    fs::write(&archive, baseline).unwrap();
    convert(&data, &source).await;
    let originals = age_source(&source, &data, 1, None);
    assert!(originals.contains_key("scripts/samearchivegood.luau"));
    assert!(originals.contains_key("scripts/archivebad.luau"));
    let mut bytes = bsa::v104(
        &[
            Entry {
                name: "scripts/samearchivegood.pex",
                data: &good,
            },
            Entry {
                name: "scripts/odd%25.pex",
                data: &odd,
            },
            Entry {
                name: "scripts/deep%2525.pex",
                data: &odd,
            },
            Entry {
                name: "scripts/archivebad.pex",
                data: &bad,
            },
        ],
        bsa::Compression::Zlib,
    )
    .unwrap();
    if corrupt_payload {
        // The fourth record is the last payload in this single-folder fixture.
        let record = 36 + 16 + 1 + "scripts".len() + 1 + 3 * 16;
        let payload =
            u32::from_le_bytes(bytes[record + 12..record + 16].try_into().unwrap()) as usize;
        assert!(payload + 8 < bytes.len());
        bytes[payload + 4..].fill(0xff);
    }
    replace_file(&archive, &bytes);
    // Inventory succeeds: each raw name has passed the extractor's first canonicalization.
    let listed = converter::archive::ArchiveExtractor::list_paths(&archive).unwrap();
    assert!(listed.contains(&"scripts/odd%.pex".into()));
    assert!(listed.contains(&"scripts/deep%25.pex".into()));
    let before = script_recovery_package_hashes(&source);
    let archive_before = hash_file(&archive).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let mut config = common::cpu_lod_config(&data, &output);
    config.fail_fast = fail_fast;
    let result = AssetPipeline::rebuild_metadata_async(config, &source, tx).await;
    drain.await.unwrap();
    assert_eq!(script_recovery_package_hashes(&source), before);
    assert_eq!(hash_file(&archive).unwrap(), archive_before);
    if fail_fast {
        assert!(
            result.is_err(),
            "explicit fail-fast must retain the member-name refusal"
        );
        assert!(!output.exists());
        return;
    }
    assert!(
        result.is_ok(),
        "unusable archive script names must preserve neighboring publication: corrupt={corrupt_payload}, result={result:?}"
    );
    let report = result.unwrap();
    assert!(!report.complete);
    assert!(report.skipped >= 2 + u64::from(corrupt_payload));
    let name_warnings: Vec<_> = report
        .warnings
        .iter()
        .filter(|warning| warning.contains("script archive members with unusable names"))
        .collect();
    assert_eq!(
        name_warnings.len(),
        1,
        "one bounded warning per archive: {:?}",
        report.warnings
    );
    assert!(name_warnings[0].contains("2 script archive members"));
    assert!(name_warnings[0].contains("Skyrim - Scripts.bsa"));
    let published = ConversionManifest::load(&output.join("conversion-manifest.json")).unwrap();
    assert!(!published.complete);
    assert!(!published.entries.contains_key("scripts/odd%.pex"));
    assert!(!published.entries.contains_key("scripts/deep%25.pex"));
    assert!(!output.join("scripts/odd%.luau").exists());
    assert!(!output.join("scripts/deep%.luau").exists());
    for (name, bytes) in &originals {
        if corrupt_payload && name == "scripts/archivebad.luau" {
            assert!(!output.join(name).exists());
            assert!(published.failures.contains_key("scripts/archivebad.pex"));
        } else {
            assert_eq!(
                &fs::read(output.join(name)).unwrap(),
                bytes,
                "healthy script {name}"
            );
        }
    }
    let original = ConversionManifest::load(&source.join("conversion-manifest.json")).unwrap();
    for entry in original
        .entries
        .values()
        .filter(|entry| entry.output.ends_with(".glb") || entry.output.ends_with(".ktx2"))
    {
        assert_eq!(
            hash_file(&output.join(&entry.output)).unwrap(),
            entry.output_hash
        );
    }
    assert!(output.join("grass_cache.rkyv").is_file());
    assert_eq!(
        fs::read(output.join("scripts/papyrus_runtime.luau")).unwrap(),
        include_bytes!("../../shared/src/papyrus_runtime.luau")
    );
    let connection = Connection::open(output.join("skyrim_world.db")).unwrap();
    let count: u64 = connection
        .query_row(
            "SELECT COUNT(*) FROM \"references\" WHERE id=4660",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "healthy neighboring world record must publish");
}

/// A successful extraction batch drops only unidentifiable names before the VFS overlay.
#[tokio::test]
async fn metadata_script_recovery_skips_odd_names_after_successful_archive_batch() {
    metadata_odd_archive_names_publication(false, false).await;
}

/// Per-member retry drops unidentifiable names while preserving authenticated healthy neighbors.
#[tokio::test]
async fn metadata_script_recovery_skips_odd_names_during_archive_member_retry() {
    metadata_odd_archive_names_publication(true, false).await;
}

/// Explicit fail-fast preserves refusal for both unusable names and broken selected payloads.
#[tokio::test]
async fn metadata_script_recovery_odd_names_respect_fail_fast() {
    metadata_odd_archive_names_publication(false, true).await;
    metadata_odd_archive_names_publication(true, true).await;
}

#[tokio::test]
async fn metadata_rebuild_populates_perk_tables_while_reusing_assets() {
    use dummy_content::{inhouse_conditions as cond, inhouse_perks as perks};
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("rebuilt");
    generate(&data);

    let skyrim_path = data.join("Skyrim.esm");
    let mut skyrim = fs::read(&skyrim_path).unwrap();
    skyrim.extend(perks::targets());
    skyrim.extend(perks::record(
        b"KYWD",
        0x901,
        0,
        &perks::subrecord(b"EDID", &perks::text("RebuildWeaponSword")),
    ));
    let mut perk = perks::prefix(10);
    perk.extend(perks::subrecord(b"PRKE", &[2, 0, 0]));
    perk.extend(perks::subrecord(b"DATA", &[0, 3, 1]));
    perk.extend(perks::subrecord(b"PRKC", &[0]));
    perk.extend(perks::subrecord(
        b"CTDA",
        &cond::condition(0, 1.0f32.to_bits(), 560, 0x901, 0, 0, 0),
    ));
    perk.extend(perks::subrecord(b"EPFT", &[1]));
    perk.extend(perks::subrecord(b"EPFD", &2.0f32.to_le_bytes()));
    perk.extend(perks::subrecord(b"PRKF", &[]));
    skyrim.extend(perks::record(b"PERK", perks::FIRST_ID, 0, &perk));
    fs::write(&skyrim_path, skyrim).unwrap();

    convert(&data, &source).await;
    // Drop the optional #599 tables from `source` to simulate a pre-#599 pack so only a fresh
    // rebuild projection can populate them in `output/skyrim_world.db`.
    let source_db = Connection::open(source.join("skyrim_world.db")).unwrap();
    source_db
        .execute_batch(
            "DROP TABLE perks;
             DROP TABLE perk_effects;
             DROP TABLE perk_entry_points;
             DROP TABLE record_conditions;",
        )
        .unwrap();
    let dropped_tables: i64 = source_db
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='perk_entry_points'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(dropped_tables, 0);
    drop(source_db);

    let report = rebuild(&data, &source, &output).await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);

    let conn = Connection::open(output.join("skyrim_world.db")).unwrap();
    let rebuilt: (i64, i64, f64, Option<u32>) = conn
        .query_row(
            "SELECT p.level, ep.effect_index, ep.value_float_1, rc.param1_form_id
             FROM perks p
             JOIN perk_entry_points ep ON ep.perk_id = p.id
             JOIN record_conditions rc
               ON rc.owner_id = ep.perk_id
              AND rc.owner_kind = 'PERK_EFFECT'
              AND rc.parent_index = ep.effect_index
             WHERE p.id = ?1 AND ep.entry_point = 0 AND ep.function = 3",
            [perks::FIRST_ID],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(rebuilt, (10, 0, 2.0, Some(0x901)));
}
