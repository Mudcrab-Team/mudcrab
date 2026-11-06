//! Bounded local bundle creation. This module never opens a browser or contacts GitHub.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use bevy::prelude::Image;
use image::{ImageEncoder, codecs::png::PngEncoder};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use url::Url;
use uuid::Uuid;
use zip::{ZipWriter, write::SimpleFileOptions};

use super::privacy::Sanitizer;

pub const MAX_LOG_BYTES: usize = 1024 * 1024;
pub const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_SCREENSHOT_PIXELS: u64 = 16_000_000;
pub const MAX_PNG_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_ZIP_BYTES: u64 = 20 * 1024 * 1024;
const MAX_FIELD_BYTES: usize = 4096;
const MAX_JSON_NODES: usize = 8192;
const MAX_JSON_DEPTH: usize = 32;
const ISSUE_URL_LIMIT: usize = 4096;
const FILES: [&str; 5] = [
    "report.json",
    "logs.txt",
    "screenshot.png",
    "report.md",
    "README.txt",
];

pub struct BundleInput {
    pub id: String,
    pub created_unix_ms: u128,
    pub started: Instant,
    pub deadline: Instant,
    pub cancelled: Arc<AtomicBool>,
    pub snapshot: Value,
    pub logs: String,
    pub screenshot: Option<Image>,
    pub screenshot_error: Option<String>,
    pub output_dir: PathBuf,
    pub sanitizer: Sanitizer,
}

#[derive(Debug)]
pub struct BundleOutcome {
    pub directory: PathBuf,
    pub archive: PathBuf,
    pub issue_url: String,
    pub partial: bool,
    pub missing: Vec<String>,
}

#[derive(Debug)]
pub struct BundleError {
    pub message: String,
    pub recovered_directory: Option<PathBuf>,
}

impl std::fmt::Display for BundleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}

impl std::error::Error for BundleError {}

#[derive(Clone)]
struct Control {
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
}

impl Control {
    fn check(&self) -> io::Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Bug report canceled",
            ));
        }
        if Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Bug report collection timed out",
            ));
        }
        Ok(())
    }
}

#[derive(Default)]
struct Component {
    bytes: u64,
    hash: Option<String>,
    reason: Option<String>,
    present: bool,
}

impl Component {
    fn metadata(&self) -> Value {
        json!({
            "status": if !self.present { "omitted" } else if self.reason.is_some() { "partial" } else { "complete" },
            "size_bytes": if self.present { Some(self.bytes) } else { None },
            "sha256": self.hash,
            "omission_reason": self.reason,
        })
    }
}

/// Resolves application-data storage without falling back to the working directory.
pub fn default_report_dir() -> Result<PathBuf, String> {
    #[cfg(target_os = "windows")]
    let root = std::env::var_os("LOCALAPPDATA")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "LOCALAPPDATA is unavailable; set --bug-report-dir".to_string())?
        .join("Mudcrab");
    #[cfg(target_os = "macos")]
    let root = home_dir()?.join("Library/Application Support/Mudcrab");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let root = match std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        Some(path) if Path::new(&path).is_absolute() => PathBuf::from(path).join("mudcrab"),
        _ => home_dir()?.join(".local/share/mudcrab"),
    };
    if !root.is_absolute() {
        return Err("Application-data directory is not absolute; set --bug-report-dir".into());
    }
    Ok(root.join("reports"))
}

#[cfg(not(target_os = "windows"))]
fn home_dir() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| "Home directory is unavailable; set --bug-report-dir".into())
}

/// Builds and validates task-owned temporary artifacts, then publishes both using
/// no-overwrite renames. A failure after the first rename preserves that directory.
pub fn publish(input: BundleInput) -> Result<BundleOutcome, BundleError> {
    publish_inner(input, |_, _, _, _| Ok(()))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PublicationPoint {
    Ready,
    DirectoryPublished,
    ArchivePublished,
}

fn publish_inner(
    mut input: BundleInput,
    mut publication_hook: impl FnMut(&Path, &Path, &Control, PublicationPoint) -> io::Result<()>,
) -> Result<BundleOutcome, BundleError> {
    let control = Control {
        deadline: input.deadline,
        cancelled: input.cancelled.clone(),
    };
    let mut recovered_directory = None;
    let mut archive_published = false;
    let result = (|| -> io::Result<BundleOutcome> {
        control.check()?;
        validate_id(&input.id)?;
        fs::create_dir_all(&input.output_dir)?;
        let directory = input.output_dir.join(&input.id);
        let archive = input.output_dir.join(format!("{}.zip", input.id));
        // This produces an early useful error; the rename itself also rejects collisions.
        for path in [&directory, &archive] {
            if fs::symlink_metadata(path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "A report with this ID already exists",
                ));
            }
        }
        control.check()?;
        let suffix = Uuid::new_v4().simple().to_string();
        let temporary_directory = input.output_dir.join(format!(".{}.{suffix}.tmp", input.id));
        let temporary_archive = input
            .output_dir
            .join(format!(".{}.{suffix}.zip.tmp", input.id));
        create_private_dir(&temporary_directory)?;
        let mut cleanup = TemporaryArtifacts {
            directory: Some(temporary_directory.clone()),
            archive: None,
        };

        let mut components = BTreeMap::<&str, Component>::new();
        for name in FILES {
            components.insert(name, Component::default());
        }
        let mut truncations = 0usize;
        let mut remaining_nodes = MAX_JSON_NODES;
        bound_value(
            &mut input.snapshot,
            0,
            &mut remaining_nodes,
            &mut truncations,
        );
        input.sanitizer.sanitize_value(&mut input.snapshot);
        control.check()?;
        let mut report_bytes = serialize_bounded(&input.snapshot, MAX_TEXT_BYTES / 2);
        let mut report_reason = (truncations != 0)
            .then(|| format!("{truncations} oversized or deeply nested snapshot values truncated"));
        if let Some(warnings) = input
            .snapshot
            .get("collection_warnings")
            .and_then(Value::as_array)
            .filter(|warnings| !warnings.is_empty())
        {
            let summary = warnings
                .iter()
                .take(3)
                .filter_map(Value::as_str)
                .map(|warning| {
                    let mut warning = sanitized_reason(warning, &input.sanitizer);
                    truncate_utf8(&mut warning, 240);
                    warning
                })
                .collect::<Vec<_>>()
                .join("; ");
            let reason = format!(
                "Unavailable runtime diagnostics: {summary}{}",
                if warnings.len() > 3 {
                    format!("; {} additional warnings", warnings.len() - 3)
                } else {
                    String::new()
                }
            );
            report_reason = Some(match report_reason {
                Some(existing) => format!("{existing}; {reason}"),
                None => reason,
            });
        }
        if report_bytes.is_err() {
            input.snapshot = json!({
                "unavailable": "Runtime snapshot exceeded the structured-text limit",
                "export": {"snapshot_omitted": true},
            });
            report_reason = Some("Runtime snapshot exceeded the structured-text limit".into());
            report_bytes = serialize_bounded(&input.snapshot, MAX_TEXT_BYTES / 2);
        }
        put_bytes(
            &temporary_directory,
            "report.json",
            &report_bytes?,
            report_reason,
            &mut components,
            &control,
        )?;

        let raw_log_bytes = input.logs.len();
        // Discard old bytes before making a sanitized copy; the ring already supplies
        // its event/time-range metadata. Preserve UTF-8 even for an oversized caller.
        let bounded_logs = tail_utf8(&input.logs, MAX_LOG_BYTES);
        let logs = input.sanitizer.sanitize(bounded_logs);
        let bounded_logs = tail_utf8(&logs, MAX_LOG_BYTES);
        let log_reason = if raw_log_bytes > MAX_LOG_BYTES || logs.len() > MAX_LOG_BYTES {
            Some(format!(
                "Log export truncated; original bytes: {raw_log_bytes}, exported bytes: {}",
                bounded_logs.len()
            ))
        } else {
            None
        };
        put_bytes(
            &temporary_directory,
            "logs.txt",
            bounded_logs.as_bytes(),
            log_reason,
            &mut components,
            &control,
        )?;
        // The snapshot and logs can together fill the text allowance. Optional logs
        // yield enough room for the required draft, README, and manifest.
        let text_used = components["report.json"].bytes + components["logs.txt"].bytes;
        if text_used > (MAX_TEXT_BYTES - 32 * 1024) as u64 {
            omit_file(
                &temporary_directory,
                "logs.txt",
                "Logs omitted to preserve the structured-text limit",
                &mut components,
            )?;
        }

        control.check()?;
        let screenshot = input.screenshot.take();
        if let Some(image) = screenshot {
            match encode_screenshot(image, &temporary_directory.join("screenshot.png"), &control) {
                Ok(()) => record_file(
                    &temporary_directory,
                    "screenshot.png",
                    None,
                    &mut components,
                    &control,
                )?,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Err(error);
                }
                Err(error) => {
                    let _ = fs::remove_file(temporary_directory.join("screenshot.png"));
                    components.get_mut("screenshot.png").unwrap().reason =
                        Some(sanitized_reason(&error.to_string(), &input.sanitizer));
                }
            }
        } else {
            components.get_mut("screenshot.png").unwrap().reason = Some(sanitized_reason(
                input
                    .screenshot_error
                    .as_deref()
                    .unwrap_or("Game-window screenshot unavailable"),
                &input.sanitizer,
            ));
        }
        control.check()?;
        let missing = missing_components(&components);
        let partial = !missing.is_empty();
        let summary = system_summary(&input.snapshot, &input.sanitizer);
        let issue_url = issue_url(&input.id, partial, &summary);
        let draft = draft(&input.id, partial, &summary, &missing);
        let readme = readme(&input.id, &missing);
        put_bytes(
            &temporary_directory,
            "report.md",
            draft.as_bytes(),
            None,
            &mut components,
            &control,
        )?;
        put_bytes(
            &temporary_directory,
            "README.txt",
            readme.as_bytes(),
            None,
            &mut components,
            &control,
        )?;

        let created = i128::try_from(input.created_unix_ms)
            .ok()
            .and_then(|ms| ms.checked_mul(1_000_000))
            .and_then(|ns| OffsetDateTime::from_unix_timestamp_nanos(ns).ok())
            .and_then(|date| date.format(&Rfc3339).ok())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Invalid report creation timestamp",
                )
            })?;
        let manifest = json!({
            "schema_version": 1,
            "report_id": input.id,
            "created_utc": created,
            "collection_duration_ms": input.started.elapsed().as_millis(),
            "status": if partial { "partial" } else { "complete" },
            "components": components.iter().map(|(name, component)| ((*name).to_string(), component.metadata())).collect::<serde_json::Map<String, Value>>(),
            "privacy": "Text is sanitized. Review all files and the screenshot before sharing; arbitrary text cannot be proven secret-free.",
        });
        let manifest_bytes = serialize_bounded(&manifest, 16 * 1024)?;
        let total_text: u64 = components
            .iter()
            .filter(|(name, _)| **name != "screenshot.png")
            .map(|(_, component)| component.bytes)
            .sum::<u64>()
            + manifest_bytes.len() as u64;
        if total_text > MAX_TEXT_BYTES as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Required report text exceeded the 2 MiB limit",
            ));
        }
        write_new(
            &temporary_directory.join("manifest.json"),
            &manifest_bytes,
            &control,
        )?;
        verify_components(&temporary_directory, &components, &control)?;
        // Acquire cleanup ownership only after exclusive creation succeeds.
        let archive_file = private_file(&temporary_archive)?;
        cleanup.archive = Some(temporary_archive.clone());
        write_archive(
            &temporary_directory,
            archive_file,
            &input.id,
            &components,
            &control,
        )?;
        verify_archive(
            &temporary_archive,
            &input.id,
            &components,
            &manifest_bytes,
            &control,
        )?;

        publication_hook(&directory, &archive, &control, PublicationPoint::Ready)?;
        control.check()?;
        rename_noreplace(&temporary_directory, &directory)?;
        cleanup.directory = None;
        recovered_directory = Some(directory.clone());
        publication_hook(
            &directory,
            &archive,
            &control,
            PublicationPoint::DirectoryPublished,
        )?;
        control.check()?;
        // The directory remains available if this step fails or the request expires.
        rename_noreplace(&temporary_archive, &archive)?;
        cleanup.archive = None;
        archive_published = true;
        publication_hook(
            &directory,
            &archive,
            &control,
            PublicationPoint::ArchivePublished,
        )?;
        control.check()?;
        Ok(BundleOutcome {
            directory,
            archive,
            issue_url,
            partial,
            missing,
        })
    })();
    result.map_err(|error| BundleError {
        message: if archive_published {
            format!("Report files preserved after collection stopped: {error}")
        } else if recovered_directory.is_some() {
            format!("Report files preserved; ZIP could not be saved: {error}")
        } else {
            error.to_string()
        },
        recovered_directory,
    })
}

fn validate_id(id: &str) -> io::Result<()> {
    if id.is_empty()
        || id.len() > 120
        || !id.as_bytes()[0].is_ascii_alphanumeric()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid bug-report ID",
        ));
    }
    Ok(())
}

fn bound_value(value: &mut Value, depth: usize, remaining: &mut usize, truncations: &mut usize) {
    if *remaining == 0 || depth > MAX_JSON_DEPTH {
        *value = Value::Null;
        *truncations += 1;
        return;
    }
    *remaining -= 1;
    match value {
        Value::String(text) if text.len() > MAX_FIELD_BYTES => {
            truncate_utf8(text, MAX_FIELD_BYTES - " [truncated]".len());
            text.push_str(" [truncated]");
            *truncations += 1;
        }
        Value::Array(values) => {
            let limit = values.len().min(*remaining);
            if values.len() > limit {
                *truncations += values.len() - limit;
                values.truncate(limit);
            }
            for value in values {
                bound_value(value, depth + 1, remaining, truncations);
            }
        }
        Value::Object(values) => {
            // Keys are bounded too, avoiding large temporary strings inside sanitization.
            let original = std::mem::take(values);
            for (mut key, mut value) in original {
                if *remaining == 0 {
                    *truncations += 1;
                    continue;
                }
                if key.len() > MAX_FIELD_BYTES {
                    truncate_utf8(&mut key, MAX_FIELD_BYTES);
                    *truncations += 1;
                }
                bound_value(&mut value, depth + 1, remaining, truncations);
                values.insert(key, value);
            }
        }
        _ => {}
    }
}

fn truncate_utf8(text: &mut String, limit: usize) {
    let mut end = limit.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
}

fn tail_utf8(text: &str, limit: usize) -> &str {
    let mut start = text.len().saturating_sub(limit);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

fn serialize_bounded(value: &Value, max_bytes: usize) -> io::Result<Vec<u8>> {
    let mut output = LimitedWriter::new(Vec::new(), max_bytes as u64, None);
    serde_json::to_writer_pretty(&mut output, value).map_err(io::Error::other)?;
    Ok(output.inner)
}

fn put_bytes(
    directory: &Path,
    name: &'static str,
    bytes: &[u8],
    reason: Option<String>,
    components: &mut BTreeMap<&str, Component>,
    control: &Control,
) -> io::Result<()> {
    write_new(&directory.join(name), bytes, control)?;
    components.insert(
        name,
        Component {
            bytes: bytes.len() as u64,
            hash: Some(format!("{:x}", Sha256::digest(bytes))),
            reason,
            present: true,
        },
    );
    Ok(())
}

fn record_file(
    directory: &Path,
    name: &'static str,
    reason: Option<String>,
    components: &mut BTreeMap<&str, Component>,
    control: &Control,
) -> io::Result<()> {
    let (bytes, hash) = file_hash(&directory.join(name), control)?;
    components.insert(
        name,
        Component {
            bytes,
            hash: Some(hash),
            reason,
            present: true,
        },
    );
    Ok(())
}

fn omit_file(
    directory: &Path,
    name: &'static str,
    reason: &str,
    components: &mut BTreeMap<&str, Component>,
) -> io::Result<()> {
    fs::remove_file(directory.join(name))?;
    components.insert(
        name,
        Component {
            reason: Some(reason.into()),
            ..Default::default()
        },
    );
    Ok(())
}

fn write_new(path: &Path, bytes: &[u8], control: &Control) -> io::Result<()> {
    control.check()?;
    let mut file = private_file(path)?;
    for chunk in bytes.chunks(64 * 1024) {
        control.check()?;
        file.write_all(chunk)?;
    }
    file.sync_all()?;
    control.check()
}

fn private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    // Only Unix sets permission bits on the builder, so only that arm needs it mutable.
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    builder.create(path)
}

fn file_hash(path: &Path, control: &Control) -> io::Result<(u64, String)> {
    control.check()?;
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Report component is not a regular file",
        ));
    }
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut size = 0;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        control.check()?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size += count as u64;
        if size > MAX_PNG_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Report component exceeds file limit",
            ));
        }
        hasher.update(&buffer[..count]);
    }
    Ok((size, format!("{:x}", hasher.finalize())))
}

fn verify_components(
    directory: &Path,
    components: &BTreeMap<&str, Component>,
    control: &Control,
) -> io::Result<()> {
    for (name, component) in components {
        if component.present {
            let (bytes, hash) = file_hash(&directory.join(name), control)?;
            if bytes != component.bytes || Some(&hash) != component.hash.as_ref() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Report component hash or size mismatch",
                ));
            }
        }
    }
    Ok(())
}

fn encode_screenshot(image: Image, path: &Path, control: &Control) -> io::Result<()> {
    let size = image.texture_descriptor.size;
    let pixels = u64::from(size.width) * u64::from(size.height);
    if size.depth_or_array_layers != 1 || pixels == 0 || pixels > MAX_SCREENSHOT_PIXELS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Screenshot omitted: dimensions exceed the 16 million pixel limit or are unsupported",
        ));
    }
    control.check()?;
    let dynamic = image.try_into_dynamic().map_err(io::Error::other)?;
    // As in Bevy's screenshot writer, discard alpha: HDR captures can store
    // brightness there rather than display transparency.
    let rgb = dynamic.into_rgb8();
    control.check()?;
    let file = private_file(path)?;
    let mut writer = LimitedWriter::new(file, MAX_PNG_BYTES, Some(control.clone()));
    PngEncoder::new(&mut writer)
        .write_image(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|error| {
            if let Err(interrupted) = control.check() {
                interrupted
            } else {
                io::Error::other(format!("Screenshot PNG encoding failed: {error}"))
            }
        })?;
    writer.inner.sync_all()?;
    control.check()
}

struct LimitedWriter<W> {
    inner: W,
    position: u64,
    limit: u64,
    control: Option<Control>,
}

impl<W> LimitedWriter<W> {
    fn new(inner: W, limit: u64, control: Option<Control>) -> Self {
        Self {
            inner,
            position: 0,
            limit,
            control,
        }
    }
    fn check(&self) -> io::Result<()> {
        if let Some(control) = &self.control {
            control.check()?;
        }
        Ok(())
    }
}

impl<W: Write> Write for LimitedWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.check()?;
        if buffer.len() as u64 > self.limit.saturating_sub(self.position) {
            return Err(io::Error::other("Bug-report file size limit exceeded"));
        }
        let count = self.inner.write(buffer)?;
        self.position += count as u64;
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.check()?;
        self.inner.flush()
    }
}

impl<W: Seek> Seek for LimitedWriter<W> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.check()?;
        let position = self.inner.seek(position)?;
        if position > self.limit {
            // A failed seek must not leave the underlying file beyond its cap.
            self.inner.seek(SeekFrom::Start(self.position))?;
            return Err(io::Error::other("Bug-report file size limit exceeded"));
        }
        self.position = position;
        Ok(position)
    }
}

fn write_archive(
    directory: &Path,
    file: File,
    id: &str,
    components: &BTreeMap<&str, Component>,
    control: &Control,
) -> io::Result<()> {
    control.check()?;
    let mut zip = ZipWriter::new(LimitedWriter::new(
        file,
        MAX_ZIP_BYTES,
        Some(control.clone()),
    ));
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .unix_permissions(0o600);
    // Fixed entry names only. Never enumerate the report, asset, or world directories.
    for name in std::iter::once("manifest.json")
        .chain(FILES.into_iter().filter(|name| components[name].present))
    {
        control.check()?;
        zip.start_file(format!("{id}/{name}"), options)
            .map_err(io::Error::other)?;
        let mut file = File::open(directory.join(name))?;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            control.check()?;
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            zip.write_all(&buffer[..count])?;
        }
    }
    let writer = zip.finish().map_err(io::Error::other)?;
    writer.inner.sync_all()?;
    if writer.inner.metadata()?.len() > MAX_ZIP_BYTES {
        return Err(io::Error::other("Bug-report ZIP exceeds the 20 MiB limit"));
    }
    control.check()
}

fn verify_archive(
    path: &Path,
    id: &str,
    components: &BTreeMap<&str, Component>,
    manifest: &[u8],
    control: &Control,
) -> io::Result<()> {
    control.check()?;
    let mut zip = zip::ZipArchive::new(File::open(path)?).map_err(io::Error::other)?;
    let expected = 1 + components
        .values()
        .filter(|component| component.present)
        .count();
    if zip.len() != expected {
        return Err(io::Error::other("Bug-report ZIP entry count mismatch"));
    }
    for name in std::iter::once("manifest.json")
        .chain(FILES.into_iter().filter(|name| components[name].present))
    {
        control.check()?;
        let mut entry = zip
            .by_name(&format!("{id}/{name}"))
            .map_err(io::Error::other)?;
        let (size, hash) = if name == "manifest.json" {
            (
                manifest.len() as u64,
                format!("{:x}", Sha256::digest(manifest)),
            )
        } else {
            (
                components[name].bytes,
                components[name].hash.as_ref().unwrap().clone(),
            )
        };
        if entry.size() != size {
            return Err(io::Error::other("Bug-report ZIP component size mismatch"));
        }
        let mut digest = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            control.check()?;
            let count = entry.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        if format!("{:x}", digest.finalize()) != hash {
            return Err(io::Error::other("Bug-report ZIP component hash mismatch"));
        }
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn rename_noreplace(source: &Path, target: &Path) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let source = CString::new(source.as_os_str().as_bytes()).map_err(io::Error::other)?;
    let target = CString::new(target.as_os_str().as_bytes()).map_err(io::Error::other)?;
    // SAFETY: both arguments are owned, valid, NUL-terminated filesystem paths.
    // Each API atomically refuses to replace an existing target.
    let result = unsafe {
        #[cfg(target_os = "linux")]
        {
            libc::renameat2(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                target.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        }
        #[cfg(target_os = "macos")]
        {
            libc::renamex_np(source.as_ptr(), target.as_ptr(), libc::RENAME_EXCL)
        }
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "windows")]
fn rename_noreplace(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let source: Vec<u16> = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let target: Vec<u16> = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    if source[..source.len() - 1].contains(&0) || target[..target.len() - 1].contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Report path contains NUL",
        ));
    }
    // SAFETY: both buffers are owned, NUL-terminated Windows paths. MoveFileW
    // refuses an occupied target for both files and directories.
    let result = unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileW(source.as_ptr(), target.as_ptr())
    };
    if result != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn rename_noreplace(_source: &Path, _target: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Atomic no-overwrite report publication is unsupported on this platform",
    ))
}

struct TemporaryArtifacts {
    directory: Option<PathBuf>,
    archive: Option<PathBuf>,
}
impl Drop for TemporaryArtifacts {
    fn drop(&mut self) {
        if let Some(archive) = &self.archive {
            let _ = fs::remove_file(archive);
        }
        if let Some(directory) = &self.directory {
            let _ = fs::remove_dir_all(directory);
        }
    }
}

fn missing_components(components: &BTreeMap<&str, Component>) -> Vec<String> {
    components
        .iter()
        .filter_map(|(name, component)| {
            component
                .reason
                .as_ref()
                .map(|reason| format!("{name}: {reason}"))
        })
        .collect()
}

fn sanitized_reason(reason: &str, sanitizer: &Sanitizer) -> String {
    let mut end = reason.len().min(1024);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    let mut reason = sanitizer
        .sanitize(&reason[..end])
        .replace(['\r', '\n', '\t'], " ");
    truncate_utf8(&mut reason, 1024);
    reason
}

fn summary_field(snapshot: &Value, pointers: &[&str], sanitizer: &Sanitizer) -> String {
    let value = pointers
        .iter()
        .find_map(|pointer| snapshot.pointer(pointer).filter(|value| !value.is_null()));
    let mut text = match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(value)) => value.to_string(),
        _ => "unknown".into(),
    };
    truncate_utf8(&mut text, 240);
    // Keep every value on one line so diagnostics cannot introduce new form sections.
    let text = sanitizer.sanitize(&text).replace(['\r', '\n', '\t'], " ");
    if ["~/", "~\\", "<assets>", "<reports>", "<path>"]
        .iter()
        .any(|marker| text.contains(marker))
    {
        "[redacted local path]".into()
    } else {
        text
    }
}

fn system_summary(snapshot: &Value, sanitizer: &Sanitizer) -> String {
    let fields = [
        ("Version", vec!["/build/version", "/build/mudcrab_version"]),
        ("Commit", vec!["/build/git_commit", "/build/commit"]),
        ("OS", vec!["/platform/os", "/platform/os_name"]),
        (
            "GPU",
            vec!["/platform/gpu/name", "/platform/gpu_name", "/platform/gpu"],
        ),
        (
            "Backend",
            vec![
                "/platform/gpu/backend",
                "/platform/rendering_backend",
                "/platform/backend",
            ],
        ),
        ("Mode", vec!["/session/mode"]),
        (
            "Scene",
            vec![
                "/session/active_space/name",
                "/session/active_space",
                "/session/cell",
                "/session/cell_identity",
            ],
        ),
    ];
    fields
        .into_iter()
        .map(|(name, pointers)| {
            format!("{name}: {}", summary_field(snapshot, &pointers, sanitizer))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn issue_url(id: &str, partial: bool, summary: &str) -> String {
    let description = format!(
        "Describe the problem here.\n\nReport ID: {id}\nCollection: {}\nReview the local files and screenshot, then attach {id}.zip manually. GitHub does not receive the bundle until you attach it.",
        if partial { "partial" } else { "complete" }
    );
    let reproduction = "1. Describe the steps leading to the problem.\n2. State the expected behavior.\n3. State what happened instead.";
    let mut url = Url::parse("https://github.com/Mudcrab-Team/mudcrab/issues/new").unwrap();
    url.query_pairs_mut()
        .append_pair("template", "01-bug-report.yml")
        .append_pair("description", &description)
        .append_pair("reproduction", reproduction)
        .append_pair("system-info", summary);
    if url.as_str().len() > ISSUE_URL_LIMIT {
        url.set_query(None);
        url.query_pairs_mut().append_pair("template", "01-bug-report.yml").append_pair("description", &format!("Describe the problem here.\n\nReport ID: {id}\nReview and attach {id}.zip. Use the saved report.md for system information and reproduction prompts."));
    }
    url.into()
}

fn draft(id: &str, partial: bool, summary: &str, missing: &[String]) -> String {
    format!(
        "# Bug report\n\n## Description\n\nDescribe the problem here.\n\n## Reproduction\n\n1. Describe the steps leading to the problem.\n2. State the expected behavior.\n3. State what happened instead.\n\n## System information\n\n{summary}\n\n## Diagnostic bundle\n\nReport ID: {id}\nCollection: {}\n\n{}\nReview the readable files and screenshot, then attach `{id}.zip` manually to the GitHub issue. The screenshot may contain names or player-visible text. Choose the title and severity before submitting.\n",
        if partial { "partial" } else { "complete" },
        if missing.is_empty() {
            "All requested components were saved.\n".into()
        } else {
            format!(
                "Missing or truncated components:\n{}\n",
                missing
                    .iter()
                    .map(|item| format!("- {item}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        }
    )
}

fn readme(id: &str, missing: &[String]) -> String {
    format!(
        "Mudcrab bug report {id}\n\nmanifest.json: collection status, sizes, and SHA-256 hashes (the manifest does not hash itself).\nreport.json: allowlisted runtime diagnostics.\nlogs.txt: sanitized recent engine logs, when available.\nscreenshot.png: the game window, when available.\nreport.md: editable issue draft.\n\n{}\nReview all files before sharing. Text redaction cannot prove arbitrary messages secret-free. The screenshot can contain player-visible names or text that text redaction cannot remove. Attach the ZIP yourself; collecting a report does not upload it.\n",
        if missing.is_empty() {
            "No components missing.\n".into()
        } else {
            format!("Missing or truncated components:\n{}\n", missing.join("\n"))
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::{
        asset::RenderAssetUsages,
        render::render_resource::{Extent3d, TextureDimension, TextureFormat},
    };
    use std::time::Duration;
    use tempfile::TempDir;

    fn input(root: &TempDir) -> BundleInput {
        let now = Instant::now();
        BundleInput {
            id: "20261013T120000Z-test".into(),
            created_unix_ms: 1_792_065_600_000,
            started: now,
            deadline: now + Duration::from_secs(30),
            cancelled: Arc::new(AtomicBool::new(false)),
            snapshot: json!({"build":{"version":"0.1.0", "git_commit":null},"platform":{"os":"macOS", "gpu":{"name":"Test & GPU", "backend":"Metal"}}, "session":{"mode":"interactive", "active_space":"Riverwood"}, "error":"/Users/player/secret.log password=SECRET_SENTINEL"}),
            logs: "INFO engine: loading meshes/桶.nif from /Users/player/secret.log\n".into(),
            screenshot: None,
            screenshot_error: Some("headless session".into()),
            output_dir: root.path().join("reports"),
            sanitizer: Sanitizer::new(vec![(PathBuf::from("/Users/player"), "~".into())]),
        }
    }

    fn screenshot() -> Image {
        Image::new(
            Extent3d {
                width: 2,
                height: 2,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            vec![0x40; 2 * 2 * 4],
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::MAIN_WORLD,
        )
    }

    #[test]
    fn valid_screenshot_produces_a_complete_bundle_and_encoded_image() {
        let root = TempDir::new().unwrap();
        let mut value = input(&root);
        value.screenshot = Some(screenshot());
        let outcome = publish(value).unwrap();
        assert!(!outcome.partial);
        assert!(outcome.missing.is_empty());
        let png = fs::read(outcome.directory.join("screenshot.png")).unwrap();
        let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (2, 2));
        assert_eq!(decoded.color(), image::ColorType::Rgb8);
        assert_eq!(
            decoded.to_rgba8().get_pixel(0, 0).0,
            [0x40, 0x40, 0x40, 0xff]
        );
        let manifest: Value =
            serde_json::from_slice(&fs::read(outcome.directory.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["status"], "complete");
        assert_eq!(
            manifest["components"]["screenshot.png"]["sha256"],
            format!("{:x}", Sha256::digest(&png))
        );
    }

    #[test]
    fn oversized_image_is_omitted_before_dynamic_conversion_or_allocation() {
        let root = TempDir::new().unwrap();
        let mut value = input(&root);
        let mut image = screenshot();
        // The tiny backing buffer remains unchanged: conversion must never start.
        image.texture_descriptor.size.width = 16_000_001;
        value.screenshot = Some(image);
        let outcome = publish(value).unwrap();
        assert!(outcome.partial);
        assert!(!outcome.directory.join("screenshot.png").exists());
        assert!(
            outcome
                .missing
                .iter()
                .any(|reason| reason.contains("16 million pixel limit"))
        );
    }

    #[test]
    fn runtime_collection_warnings_mark_snapshot_and_manifest_partial() {
        let root = TempDir::new().unwrap();
        let mut value = input(&root);
        value.screenshot = Some(screenshot());
        value.snapshot["collection_warnings"] = json!([
            "camera: no camera entity",
            "active_space: runtime identity unavailable"
        ]);
        let outcome = publish(value).unwrap();
        assert!(outcome.partial);
        assert!(
            outcome
                .missing
                .iter()
                .any(|reason| reason.starts_with("report.json: Unavailable runtime diagnostics"))
        );
        let manifest: Value =
            serde_json::from_slice(&fs::read(outcome.directory.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["components"]["report.json"]["status"], "partial");
        assert!(
            manifest["components"]["report.json"]["omission_reason"]
                .as_str()
                .unwrap()
                .contains("no camera entity")
        );
    }

    #[test]
    fn publishes_matching_hashes_archive_and_safe_prefilled_fields() {
        let root = TempDir::new().unwrap();
        let outcome = publish(input(&root)).unwrap();
        assert!(outcome.partial);
        let manifest: Value =
            serde_json::from_slice(&fs::read(outcome.directory.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["schema_version"], 1);
        assert_eq!(manifest["status"], "partial");
        assert_eq!(
            manifest["components"]["screenshot.png"]["status"],
            "omitted"
        );
        let mut archive = zip::ZipArchive::new(File::open(&outcome.archive).unwrap()).unwrap();
        assert_eq!(archive.len(), 5);
        for name in [
            "manifest.json",
            "report.json",
            "logs.txt",
            "report.md",
            "README.txt",
        ] {
            let disk = fs::read(outcome.directory.join(name)).unwrap();
            let mut entry = archive
                .by_name(&format!("20261013T120000Z-test/{name}"))
                .unwrap();
            assert_eq!(entry.compression(), zip::CompressionMethod::Stored);
            let mut zipped = Vec::new();
            entry.read_to_end(&mut zipped).unwrap();
            assert_eq!(zipped, disk);
            if name != "manifest.json" {
                assert_eq!(manifest["components"][name]["size_bytes"], disk.len());
                assert_eq!(
                    manifest["components"][name]["sha256"],
                    format!("{:x}", Sha256::digest(&disk))
                );
            }
            let text = String::from_utf8(disk).unwrap();
            assert!(!text.contains("SECRET_SENTINEL"));
            assert!(!text.contains("/Users/player"));
        }
        let url = Url::parse(&outcome.issue_url).unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("github.com"));
        assert_eq!(url.path(), "/Mudcrab-Team/mudcrab/issues/new");
        let query: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query["template"], "01-bug-report.yml");
        assert!(query["system-info"].contains("Test & GPU"));
        assert!(!query.contains_key("title"));
        assert!(!query.contains_key("labels"));
        assert!(!outcome.issue_url.contains("SECRET_SENTINEL"));
        assert!(!outcome.issue_url.contains("secret.log"));
        assert!(!outcome.issue_url.contains("Users"));
    }

    #[test]
    fn collision_after_preparation_preserves_directory_without_overwriting_zip() {
        let root = TempDir::new().unwrap();
        let error = publish_inner(input(&root), |_, archive, _, point| {
            if point == PublicationPoint::Ready {
                fs::write(archive, b"existing archive")?;
            }
            Ok(())
        })
        .unwrap_err();
        let recovered = error.recovered_directory.unwrap();
        assert!(recovered.join("manifest.json").exists());
        assert_eq!(
            fs::read(recovered.with_extension("zip")).unwrap(),
            b"existing archive"
        );
        assert!(
            error
                .message
                .starts_with("Report files preserved; ZIP could not be saved")
        );
        assert_eq!(
            fs::read_dir(recovered.parent().unwrap()).unwrap().count(),
            2
        );
    }

    #[test]
    fn directory_collision_is_atomic_and_existing_data_survives() {
        let root = TempDir::new().unwrap();
        let error = publish_inner(input(&root), |directory, _, _, point| {
            if point == PublicationPoint::Ready {
                fs::create_dir(directory)?;
                fs::write(directory.join("owner.txt"), b"keep me")?;
            }
            Ok(())
        })
        .unwrap_err();
        assert!(error.recovered_directory.is_none());
        assert_eq!(
            fs::read(root.path().join("reports/20261013T120000Z-test/owner.txt")).unwrap(),
            b"keep me"
        );
        assert_eq!(
            fs::read_dir(root.path().join("reports")).unwrap().count(),
            1
        );
    }

    #[test]
    fn cancellation_before_publication_leaves_no_artifacts() {
        let root = TempDir::new().unwrap();
        let error = publish_inner(input(&root), |_, _, control, point| {
            if point == PublicationPoint::Ready {
                control.cancelled.store(true, Ordering::Release);
            }
            Ok(())
        })
        .unwrap_err();
        assert_eq!(error.message, "Bug report canceled");
        assert!(error.recovered_directory.is_none());
        assert_eq!(
            fs::read_dir(root.path().join("reports")).unwrap().count(),
            0
        );
        let mut expired = input(&root);
        expired.deadline = Instant::now();
        assert_eq!(
            publish(expired).unwrap_err().message,
            "Bug report collection timed out"
        );
    }

    #[test]
    fn cleanup_never_claims_an_archive_that_exclusive_creation_did_not_create() {
        let root = TempDir::new().unwrap();
        let directory = root.path().join("owned.tmp");
        let foreign_archive = root.path().join("foreign.zip.tmp");
        create_private_dir(&directory).unwrap();
        fs::write(&foreign_archive, b"foreign data").unwrap();
        {
            let _cleanup = TemporaryArtifacts {
                directory: Some(directory.clone()),
                archive: None,
            };
            assert!(private_file(&foreign_archive).is_err());
        }
        assert!(!directory.exists());
        assert_eq!(fs::read(&foreign_archive).unwrap(), b"foreign data");
    }

    #[cfg(unix)]
    #[test]
    fn private_directories_are_owner_only_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let root = TempDir::new().unwrap();
        let directory = root.path().join("private");
        create_private_dir(&directory).unwrap();
        let mode = fs::metadata(&directory).unwrap().permissions().mode();
        // The process umask can only remove bits, so group and other access must stay off.
        assert_eq!(mode & 0o077, 0, "group or other access on {mode:o}");
    }

    #[test]
    fn cancellation_after_directory_publication_preserves_recovery_without_starting_zip_rename() {
        let root = TempDir::new().unwrap();
        let error = publish_inner(input(&root), |_, _, control, point| {
            if point == PublicationPoint::DirectoryPublished {
                control.cancelled.store(true, Ordering::Release);
            }
            Ok(())
        })
        .unwrap_err();
        let directory = error.recovered_directory.unwrap();
        assert!(directory.join("manifest.json").exists());
        assert!(!directory.with_extension("zip").exists());
        assert_eq!(
            fs::read_dir(directory.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn expired_completion_after_archive_publication_preserves_both_artifacts_without_saved_result()
    {
        let root = TempDir::new().unwrap();
        let error = publish_inner(input(&root), |_, _, control, point| {
            if point == PublicationPoint::ArchivePublished {
                control.cancelled.store(true, Ordering::Release);
            }
            Ok(())
        })
        .unwrap_err();
        let directory = error.recovered_directory.unwrap();
        assert!(directory.join("manifest.json").exists());
        assert!(directory.with_extension("zip").exists());
        assert!(
            error
                .message
                .starts_with("Report files preserved after collection stopped")
        );
    }

    #[test]
    fn log_and_snapshot_limits_hold_and_preserve_utf8() {
        let root = TempDir::new().unwrap();
        let mut oversized = input(&root);
        oversized.logs = "桶".repeat(MAX_LOG_BYTES);
        oversized.snapshot["large"] = Value::String("桶".repeat(MAX_TEXT_BYTES));
        let outcome = publish(oversized).unwrap();
        let report = fs::read_to_string(outcome.directory.join("report.json")).unwrap();
        assert!(report.len() < MAX_FIELD_BYTES + 2048);
        assert!(report.contains("[truncated]"));
        let logs = fs::read_to_string(outcome.directory.join("logs.txt")).unwrap();
        assert!(logs.len() <= MAX_LOG_BYTES);
        assert!(
            outcome
                .missing
                .iter()
                .any(|item| item.starts_with("logs.txt:"))
        );
        assert!(
            outcome
                .missing
                .iter()
                .any(|item| item.starts_with("report.json:"))
        );
        let text_size: u64 = fs::read_dir(&outcome.directory)
            .unwrap()
            .map(|entry| entry.unwrap().metadata().unwrap().len())
            .sum();
        assert!(text_size <= MAX_TEXT_BYTES as u64);
    }

    #[test]
    fn oversized_serialization_and_png_writer_fail_before_exceeding_the_cap() {
        let value = json!({"large":"x".repeat(100)});
        assert!(serialize_bounded(&value, 32).is_err());
        let mut writer = LimitedWriter::new(Vec::new(), 4, None);
        writer.write_all(b"1234").unwrap();
        assert!(writer.write_all(b"5").is_err());
        assert_eq!(writer.inner, b"1234");
        let mut seek_writer = LimitedWriter::new(io::Cursor::new(Vec::new()), 4, None);
        assert!(seek_writer.seek(SeekFrom::Start(100)).is_err());
        seek_writer.write_all(b"1234").unwrap();
        assert_eq!(seek_writer.inner.into_inner(), b"1234");
    }

    #[test]
    fn oversized_url_retains_report_id_and_draft_instruction() {
        let url = issue_url("safe-report-id", true, &"界".repeat(2000));
        assert!(url.len() <= ISSUE_URL_LIMIT);
        let pairs: BTreeMap<_, _> = Url::parse(&url)
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect();
        assert_eq!(pairs["template"], "01-bug-report.yml");
        assert!(pairs["description"].contains("safe-report-id"));
        assert!(pairs["description"].contains("report.md"));
        assert!(!pairs.contains_key("system-info"));
    }

    #[test]
    fn summary_cannot_send_known_root_suffixes_as_local_paths() {
        let sanitizer = Sanitizer::new(vec![(PathBuf::from("/Users/player"), "~".into())]);
        let summary = system_summary(
            &json!({"platform":{"gpu":"/Users/player/private/file.log"}}),
            &sanitizer,
        );
        assert!(summary.contains("GPU: [redacted local path]"));
        let url = issue_url("safe-id", true, &summary);
        assert!(!url.contains("private"));
        assert!(!url.contains("file.log"));
    }

    #[test]
    fn report_ids_cannot_create_nested_entries() {
        let root = TempDir::new().unwrap();
        for bad_id in ["", "../escape", "a/b", "a\\b", "-hidden", "a.zip/"] {
            let mut value = input(&root);
            value.id = bad_id.into();
            assert_eq!(publish(value).unwrap_err().message, "Invalid bug-report ID");
        }
        assert!(!root.path().join("reports").exists());
    }
}
