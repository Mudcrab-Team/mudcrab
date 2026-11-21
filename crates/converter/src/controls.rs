//! Optional, byte-preserving PC control files with effective-provider provenance.

use crate::{
    archive::{ArchiveExtractor, ExtractedFile},
    cache::hash_bytes,
    pipeline::PipelineReport,
};
use color_eyre::{Result, eyre::eyre};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub(crate) const PATHS: [&str; 3] = [
    "interface/controls/pc/controlmap.txt",
    "interface/controls/pc/keyboard_english.txt",
    "interface/controls/pc/gamepad.txt",
];

/// A winning input path, kept separate from the portable provider description.
pub(crate) struct Source {
    pub(crate) path: PathBuf,
    pub(crate) provider: String,
    expected_sha256: Option<String>,
}

/// Accept exactly the three optional runtime control files, case-insensitively.
pub(crate) fn input_key(path: &Path) -> Option<String> {
    let key = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    PATHS.contains(&key.as_str()).then_some(key)
}

/// Report whether this module publishes the path, so inventory does not count it as ignored.
pub(crate) fn owns_input(path: &Path) -> bool {
    input_key(path).is_some()
}

/// Archives arrive in load order; each later provider replaces the earlier one.
pub(crate) fn record_archive(
    sources: &mut BTreeMap<String, Source>,
    archive: &str,
    files: &[ExtractedFile],
    vfs: &Path,
) {
    for file in files {
        if let Some(key) = input_key(&file.path) {
            sources.insert(
                key,
                Source {
                    path: vfs.join(&file.path),
                    provider: format!("archive:{archive}"),
                    expected_sha256: Some(file.sha256.clone()),
                },
            );
        }
    }
}

/// Extract each optional control file independently, recording every readable winner.
/// Failed files retain their earlier providers and return at most one warning per path.
/// An unreadable archive returns one diagnostic separately so callers can avoid duplicates.
pub(crate) fn extract_archive(
    sources: &mut BTreeMap<String, Source>,
    archive: &Path,
    provider: &str,
    scratch: &Path,
) -> std::result::Result<Vec<String>, String> {
    let paths = match ArchiveExtractor::list_runtime_paths(archive) {
        Ok(paths) => paths,
        Err(error) => {
            return Err(format!(
                "{provider}: control input extraction failed: {error:#}"
            ));
        }
    };
    let mut warnings = Vec::new();
    for key in PATHS {
        let path = PathBuf::from(key);
        if !paths.contains(&path) {
            continue;
        }
        let selected = BTreeSet::from([path]);
        match ArchiveExtractor::extract_runtime_paths(archive, scratch, &selected) {
            Ok(files) => record_archive(sources, provider, &files, scratch),
            Err(error) => warnings.push(format!(
                "{provider}:{key}: control input extraction failed: {error:#}"
            )),
        }
    }
    Ok(warnings)
}

/// Loose winners (including MO2-resolved paths) override every archive provider.
pub(crate) fn record_loose(sources: &mut BTreeMap<String, Source>, relative: &Path, path: &Path) {
    if let Some(key) = input_key(relative) {
        sources.insert(
            key.clone(),
            Source {
                path: path.to_owned(),
                provider: format!("loose:{key}"),
                expected_sha256: None,
            },
        );
    }
}

#[derive(Serialize)]
struct Provenance {
    provider: String,
    source_sha256: String,
    output_sha256: String,
    skipped_lines: Vec<usize>,
}

/// Publish only usable lines, retaining source encoding, separators, comments and line endings.
/// Input-data errors warn and omit only the affected line/file; output I/O errors propagate.
pub(crate) fn publish(
    staging: &Path,
    sources: BTreeMap<String, Source>,
    report: &mut PipelineReport,
) -> Result<()> {
    let mut provenance = BTreeMap::new();
    for (key, source) in sources {
        let bytes = match fs::read(&source.path) {
            Ok(bytes) => bytes,
            Err(error) => {
                report.warnings.push(format!(
                    "{key}: unable to read winning control input: {error}"
                ));
                continue;
            }
        };
        let source_sha256 = hash_bytes(&bytes);
        if source
            .expected_sha256
            .as_ref()
            .is_some_and(|expected| *expected != source_sha256)
        {
            report.warnings.push(format!("{key}: winning archive input was overwritten by an incomplete provider; file omitted"));
            continue;
        }
        let (output, skipped_lines) = sanitize(&key, &bytes);
        if let Some(first) = skipped_lines.first() {
            report.warnings.push(format!(
                "{key}:{first}: skipped {} malformed control lines; first at line {first}",
                skipped_lines.len()
            ));
        }
        replace(&staging.join(&key), &output)?;
        report.artifacts.push(PathBuf::from(&key));
        provenance.insert(
            key,
            Provenance {
                provider: source.provider,
                source_sha256,
                output_sha256: hash_bytes(&output),
                skipped_lines,
            },
        );
    }
    if !provenance.is_empty() {
        let path = "interface/controls/pc/provenance.json";
        replace(
            &staging.join(path),
            &serde_json::to_vec_pretty(&provenance)?,
        )?;
        report.artifacts.push(PathBuf::from(path));
    }
    Ok(())
}

/// Atomically replace a resumed file without writing through a previous pack's hard link.
fn replace(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| eyre!("control output has no parent"))?;
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.persist(path)?;
    Ok(())
}

/// Skip an entire malformed physical line, never replacing it with a context separator.
/// Complete a retained CR with LF if removal would merge it with a later blank LF line.
fn sanitize(key: &str, bytes: &[u8]) -> (Vec<u8>, Vec<usize>) {
    let mut output = Vec::with_capacity(bytes.len());
    let mut skipped = Vec::new();
    for (index, line) in physical_lines(bytes).enumerate() {
        let content = if index == 0 {
            line.strip_prefix(b"\xef\xbb\xbf").unwrap_or(line)
        } else {
            line
        };
        let comment = content
            .windows(2)
            .position(|pair| pair == b"//")
            .unwrap_or(content.len());
        let content = trim(&content[..comment]);
        if content.is_empty() || valid_line(key, content) {
            // These endings were separate in the input; preserve the blank physical line.
            if output.last() == Some(&b'\r') && line.first() == Some(&b'\n') {
                output.push(b'\n');
            }
            output.extend_from_slice(line);
        } else {
            skipped.push(index + 1);
        }
    }
    (output, skipped)
}

/// Retain LF, CRLF and CR endings without counting the CRLF pair as two lines.
fn physical_lines(mut bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    std::iter::from_fn(move || {
        if bytes.is_empty() {
            return None;
        }
        let end = bytes
            .iter()
            .position(|byte| matches!(*byte, b'\r' | b'\n'))
            .map(|index| {
                index
                    + 1
                    + usize::from(bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n'))
            })
            .unwrap_or(bytes.len());
        let (line, rest) = bytes.split_at(end);
        bytes = rest;
        Some(line)
    })
}

/// Trim ASCII alignment bytes without interpreting legacy Windows-1252 key names.
fn trim(bytes: &[u8]) -> &[u8] {
    bytes.trim_ascii()
}

/// Check the structural record contract while retaining modded alias/chord binding syntax.
fn valid_line(key: &str, line: &[u8]) -> bool {
    if line
        .iter()
        .any(|byte| byte.is_ascii_control() && *byte != b'\t')
    {
        return false;
    }
    let fields: Vec<_> = line
        .split(|byte| *byte == b'\t')
        .map(trim)
        .filter(|field| !field.is_empty())
        .collect();
    if key.ends_with("/controlmap.txt") {
        matches!(fields.len(), 7 | 8)
            && fields[4..7]
                .iter()
                .all(|field| matches!(*field, b"0" | b"1"))
            && (fields.len() == 7 || hex(fields[7]))
    } else {
        let count_ok = if key.ends_with("/keyboard_english.txt") {
            matches!(fields.len(), 2 | 3)
        } else {
            fields.len() == 2
        };
        count_ok && hex(fields[1]) && (fields.len() == 2 || decimal(fields[2]))
    }
}

/// Hardware codes and event flags are unsigned 32-bit hexadecimal values.
fn hex(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|value| {
            value
                .strip_prefix("0x")
                .or_else(|| value.strip_prefix("0X"))
        })
        .is_some_and(|value| u32::from_str_radix(value, 16).is_ok())
}

/// Optional Flash codes are unsigned decimal values.
fn decimal(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes)
        .ok()
        .is_some_and(|value| value.parse::<u32>().is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keep modded binding expressions, context separators, BOM and unterminated final lines.
    #[test]
    fn structural_validation_preserves_binding_expressions_and_encoding() {
        let bytes = b"\xef\xbb\xbf// synthetic controls\r\nSynth Event\t\t0x11+0x22,0x33\t!0,Other Event\t!1,Another Event\t1\t0\t1\t0x812\r\n \t \r\nSecond\t0x17\t0xff\t0x1000\t0\t1\t1";
        assert_eq!(sanitize(PATHS[0], bytes), (bytes.to_vec(), vec![]));
        let keyboard = b"Synth\xa5\t0x17\t23\r\nOther Name\t0x33\r\n";
        assert_eq!(sanitize(PATHS[1], keyboard), (keyboard.to_vec(), vec![]));
    }

    /// Malformed shape, remap flags, numeric columns and control bytes each lose just one line.
    #[test]
    fn malformed_lines_are_removed_without_creating_contexts() {
        let input = b"bad row\r\nOne\t0x17\t0xff\t0x1000\t1\t2\t1\r\nTwo\t0x17\t0xff\t0x1000\t1\t0\t1\tbad\r\nThree\t0x17\t0xff\t0x1000\t1\t0\t1\r\n\r\nFour\t0x17\t0xff\t0x1000\t1\t0\t1\0\r\n";
        let expected = b"Three\t0x17\t0xff\t0x1000\t1\t0\t1\r\n\r\n";
        assert_eq!(
            sanitize(PATHS[0], input),
            (expected.to_vec(), vec![1, 2, 3, 6])
        );
        assert_eq!(
            sanitize(
                PATHS[1],
                b"Bad\tnohex\r\nBadFlash\t0x01\twrong\r\nGood\t0x19\t25\r\n"
            ),
            (b"Good\t0x19\t25\r\n".to_vec(), vec![1, 2])
        );
        assert_eq!(
            sanitize(PATHS[2], b"Bad\t0xnot\r\nGood\t0x1000\r\n"),
            (b"Good\t0x1000\r\n".to_vec(), vec![1])
        );
    }

    /// A large malformed input emits one summary while retaining valid neighbors and full provenance.
    #[test]
    fn many_malformed_lines_produce_one_warning_per_file() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("input.txt");
        let first = b"First\t0x11\t0xff\t0x1000\t1\t0\t1\r\n";
        let last = b"Last\t0x12\t0xff\t0x1000\t1\t0\t1\r\n";
        let mut bytes = first.to_vec();
        bytes.extend(b"bad row\r\n".repeat(100_000));
        bytes.extend_from_slice(last);
        fs::write(&input, bytes).unwrap();
        let mut sources = BTreeMap::new();
        record_loose(&mut sources, Path::new(PATHS[0]), &input);
        let mut report = PipelineReport::default();
        publish(temp.path(), sources, &mut report).unwrap();
        assert_eq!(
            report.warnings,
            vec![format!(
                "{}:2: skipped 100000 malformed control lines; first at line 2",
                PATHS[0]
            )]
        );
        assert_eq!(
            fs::read(temp.path().join(PATHS[0])).unwrap(),
            [first.as_slice(), last.as_slice()].concat()
        );
        let provenance: serde_json::Value = serde_json::from_slice(
            &fs::read(temp.path().join("interface/controls/pc/provenance.json")).unwrap(),
        )
        .unwrap();
        let skipped = provenance[PATHS[0]]["skipped_lines"].as_array().unwrap();
        assert_eq!(skipped.len(), 100_000);
        assert_eq!(skipped.first().unwrap().as_u64(), Some(2));
        assert_eq!(skipped.last().unwrap().as_u64(), Some(100_001));
    }

    /// CR-only mod files keep their rows, and mixed endings retain original line numbers.
    #[test]
    fn physical_line_endings_preserve_neighbors_and_diagnostics() {
        let bytes = b"One\t0x11\rbad\rTwo\t0x12\r\nThree\t0x13\nFour\t0x14";
        assert_eq!(
            sanitize(PATHS[2], bytes),
            (
                b"One\t0x11\rTwo\t0x12\r\nThree\t0x13\nFour\t0x14".to_vec(),
                vec![2]
            )
        );
    }

    /// Removing a row between a CR and LF must retain the blank context separator.
    #[test]
    fn review_741_removal_preserves_blank_context_after_cr() {
        let (output, skipped) = sanitize(PATHS[2], b"One\t0x11\rbad\n\nTwo\t0x12\n");
        assert_eq!(skipped, vec![2]);
        assert_eq!(
            output, b"One\t0x11\r\n\nTwo\t0x12\n",
            "removing a bad row must preserve the blank context after a CR"
        );
    }

    /// Stale optional files in resumed staging are not selected as runtime artifacts.
    #[test]
    fn absent_inputs_do_not_advertise_stale_staging_files() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("interface/controls/pc")).unwrap();
        fs::write(temp.path().join(PATHS[0]), b"stale").unwrap();
        let mut report = PipelineReport::default();
        publish(temp.path(), BTreeMap::new(), &mut report).unwrap();
        assert!(report.artifacts.is_empty());
        assert!(report.warnings.is_empty());
    }

    /// A partial failed archive cannot be attributed to a previous successful provider.
    #[test]
    fn overwritten_archive_bytes_warn_and_omit_the_file() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("input.txt");
        fs::write(&input, b"different provider").unwrap();
        let sources = BTreeMap::from([(
            PATHS[0].to_string(),
            Source {
                path: input,
                provider: "archive:previous.bsa".into(),
                expected_sha256: Some(hash_bytes(b"previous provider")),
            },
        )]);
        let mut report = PipelineReport::default();
        publish(temp.path(), sources, &mut report).unwrap();
        assert!(report.artifacts.is_empty());
        assert_eq!(report.warnings.len(), 1);
        assert!(report.warnings[0].contains("incomplete provider"));
    }
}
