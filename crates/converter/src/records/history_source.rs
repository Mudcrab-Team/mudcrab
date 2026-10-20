//! Source/physical history access, including NULL runtime IDs, without typed runtime decoding.
use super::{history, source_identity};
use color_eyre::{Result, eyre::ensure};
use rusqlite::{Connection, OpenFlags, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

const RAW_PAYLOAD_BOUND: u32 = 64 * 1024 * 1024;
const QUERY_VERSION_BOUND: usize = 4096;

/// Query a physical header or proven owner/full-local source chain, including NULL runtime IDs.
/// Supplier IDs and offsets refer to the pinned producing database, not a new runtime namespace.
#[derive(Clone, Debug, Serialize)]
pub enum SourceQuery {
    Physical {
        plugin_id: u32,
        source_record_offset: u64,
    },
    Source {
        original_owner: String,
        full_local: u32,
    },
}

/// Exact stored bytes. Compressed payload includes its native length prefix.
/// No inflation, subrecord interpretation, ID remapping, flag repair or normalization.
/// Context and checksum authority come from a caller-authenticated producing bundle.
#[derive(Debug, Serialize)]
pub struct RawSourceMaterial {
    pub header: [u8; 24],
    pub stored_payload: Option<Vec<u8>>,
    pub source_checksum_sha256: String,
    pub header_sha256: String,
    pub stored_payload_sha256: Option<String>,
    pub frame_complete: bool,
    pub access_status: &'static str,
}

/// One stored candidate with separate raw/admitted/source/runtime observations.
/// Native owner and enclosing context rely on authenticated producing SQL metadata.
/// Exact raw access does not establish typed decoding or executable wide-ID support.
#[derive(Debug, Serialize)]
pub struct SourceVersion {
    pub runtime_form_id: Option<u32>,
    pub plugin_id: u32,
    pub supplying_plugin: String,
    pub source_form_id: u32,
    pub record_type: String,
    pub record_signature: [u8; 4],
    pub flags: u32,
    pub source_record_offset: u64,
    pub source_record_size: u32,
    pub source_group_end: u64,
    pub frame_complete: bool,
    pub identity: source_identity::SourceIdentity,
    pub status: String,
    pub rejection_reason: Option<String>,
    pub source_cell_form_id: Option<u32>,
    pub source_worldspace_form_id: Option<u32>,
    pub source_topic_form_id: Option<u32>,
    pub topic_group_offset: Option<u64>,
    /// Latest physical candidate for this proven source identity, including skipped/deleted.
    pub latest_source_candidate: bool,
    /// Latest reader-admitted candidate at this source identity, independently of raw latest.
    pub latest_reader_admitted_candidate: bool,
    pub accepted_source_winner: bool,
    pub runtime_record_present: bool,
    pub runtime_fallback_before_latest_source: bool,
    pub raw: Option<RawSourceMaterial>,
    pub raw_access_failure: Option<String>,
}

/// Completion of this raw-access query only; check it before accepting emitted version rows.
/// Raw completeness is not semantic support, runtime preservation or whole-export completeness.
#[derive(Debug, Serialize)]
pub struct SourceSummary {
    pub query: SourceQuery,
    pub versions: usize,
    pub raw_complete: usize,
    pub header_only: usize,
    pub unavailable: usize,
    pub fully_raw_available: bool,
    pub typed_runtime_decoding_attempted: bool,
}

/// Bound each checksum pass to one fixed buffer and the already-open original handle.
fn digest(file: &mut File) -> Result<Vec<u8>> {
    file.seek(SeekFrom::Start(0))?;
    let mut h = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        h.update(&buffer[..n]);
    }
    Ok(h.finalize().to_vec())
}

/// Same-handle checksum guards surround exact original header/stored-body reads.
pub(crate) fn guarded_raw(
    path: &Path,
    expected: &[u8],
    version: &SourceVersion,
) -> Result<RawSourceMaterial> {
    ensure!(expected.len() == 32, "missing original checksum authority");
    let mut file = File::open(path)?;
    ensure!(
        digest(&mut file)? == expected,
        "source changed before raw access"
    );
    let file_len = file.metadata()?.len();
    ensure!(
        version.source_group_end <= file_len,
        "stored containing-group bound exceeds source"
    );
    let body_offset = version
        .source_record_offset
        .checked_add(24)
        .ok_or_else(|| color_eyre::eyre::eyre!("source offset overflow"))?;
    ensure!(
        body_offset <= version.source_group_end,
        "header exceeds containing-group bound"
    );
    file.seek(SeekFrom::Start(version.source_record_offset))?;
    let mut header = [0u8; 24];
    file.read_exact(&mut header)?;
    let word = |at| u32::from_le_bytes(header[at..at + 4].try_into().expect("fixed header"));
    ensure!(
        header[..4] == version.record_signature
            && word(4) == version.source_record_size
            && word(8) == version.flags
            && word(12) == version.source_form_id,
        "stored header differs from original"
    );
    let fits_group =
        u64::from(version.source_record_size) <= version.source_group_end - body_offset;
    ensure!(
        fits_group == version.frame_complete,
        "stored frame completeness differs from original bound"
    );
    let can_read = fits_group && version.source_record_size <= RAW_PAYLOAD_BOUND;
    let payload = if can_read {
        let mut bytes = vec![0; version.source_record_size as usize];
        file.read_exact(&mut bytes)?;
        Some(bytes)
    } else {
        None
    };
    ensure!(
        digest(&mut file)? == expected,
        "source changed after raw access"
    );
    Ok(RawSourceMaterial {
        header,
        source_checksum_sha256: expected.iter().map(|b| format!("{b:02x}")).collect(),
        header_sha256: crate::cache::hash_bytes(&header),
        stored_payload_sha256: payload.as_ref().map(|b| crate::cache::hash_bytes(b)),
        stored_payload: payload,
        frame_complete: fits_group,
        access_status: if !fits_group {
            "header_only_incomplete_frame"
        } else if !can_read {
            "header_only_payload_capacity"
        } else {
            "exact_native_header_and_stored_payload"
        },
    })
}

/// Older producers used ASCII-only folding. Their exact former key can be derived from
/// the authenticated stored plugin basename, without broadening source ownership.
fn legacy_owner_key(owner: &str, sources: &[history::PluginSource]) -> String {
    sources
        .iter()
        .find(|(name, _, _)| crate::esm::load_order::normalize_plugin_name(name) == owner)
        .map(|(name, _, _)| name.to_ascii_lowercase())
        .unwrap_or_else(|| owner.to_owned())
}

/// Find a raw or admitted latest candidate only within a proven owner/full-local key.
/// Both saved key spellings refer to the same validated plugin-basename namespace.
fn latest(
    conn: &Connection,
    identity: &source_identity::SourceIdentity,
    sources: &[history::PluginSource],
    accepted_only: bool,
) -> Result<Option<(u32, u64)>> {
    if identity.state != source_identity::SourceIdentityState::Resolved {
        return Ok(None);
    }
    let Some(owner) = identity.original_owner.as_deref() else {
        return Ok(None);
    };
    let sql = if accepted_only {
        "SELECT plugin_id,source_record_offset FROM record_overrides WHERE original_owner IN (?1,?3) AND full_local=?2 AND source_identity_state='resolved' AND status='accepted' ORDER BY plugin_id DESC,source_record_offset DESC LIMIT 1"
    } else {
        "SELECT plugin_id,source_record_offset FROM record_overrides WHERE original_owner IN (?1,?3) AND full_local=?2 AND source_identity_state='resolved' ORDER BY plugin_id DESC,source_record_offset DESC LIMIT 1"
    };
    use rusqlite::OptionalExtension;
    Ok(conn
        .query_row(
            sql,
            params![owner, identity.full_local, legacy_owner_key(owner, sources)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

/// Stream one bounded raw body at a time; callers must consume/drop rows to retain that bound.
/// Unavailable/changed originals remain local diagnostics, never material or typed proof.
///
/// # Integrity prerequisite
/// The caller must authenticate the complete database against a trusted completed-producer
/// receipt/pin and keep that integrity through this call. Read-only SQLite access does not
/// authenticate edited SQL. Original-file checksums authenticate read bytes, not stored MAST,
/// slot, group-bound, admission or winner metadata; this API does not independently reconstruct
/// those contexts from arbitrary databases. Retain before/after bundle guards with the result.
/// Stored MAST authority comes from ordered scalar rows in new packs, with legacy pack reading
/// supported by the shared history loader; ordinal validation does not authenticate that authority.
///
/// # Completion and errors
/// Accept the result only after a returned summary reports full raw availability. SQL/metadata
/// errors, callback errors and query-capacity excess return an error after possible partial
/// delivery. Per-version original access failures remain in rows and make the summary partial.
/// Missing-master producer admission and typed/runtime wide-ID decoding are not added here.
pub fn visit_source_history(
    pack: &Path,
    data: Option<&Path>,
    query: SourceQuery,
    mut receive: impl FnMut(SourceVersion) -> Result<()>,
) -> Result<SourceSummary> {
    let database = if pack.is_file() {
        pack.to_path_buf()
    } else {
        pack.join("skyrim_world.db")
    };
    let conn = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let (order, sources) = history::stored_order(&conn)?;
    let mut summary = SourceSummary {
        query: query.clone(),
        versions: 0,
        raw_complete: 0,
        header_only: 0,
        unavailable: 0,
        fully_raw_available: false,
        typed_runtime_decoding_attempted: false,
    };
    let select = "SELECT form_id,plugin_id,source_form_id,record_type,flags,source_record_offset,source_record_size,status,reason,source_cell_form_id,source_worldspace_form_id,source_topic_form_id,topic_group_offset,original_owner,full_local,source_identity_state,source_group_end,frame_complete,record_signature FROM record_overrides";
    let (sql, values): (String, Vec<rusqlite::types::Value>) = match &query {
        SourceQuery::Physical {
            plugin_id,
            source_record_offset,
        } => {
            ensure!(
                *source_record_offset <= i64::MAX as u64,
                "offset is outside SQL range"
            );
            (
                format!("{select} WHERE plugin_id=?1 AND source_record_offset=?2"),
                vec![
                    i64::from(*plugin_id).into(),
                    (*source_record_offset as i64).into(),
                ],
            )
        }
        SourceQuery::Source {
            original_owner,
            full_local,
        } => {
            ensure!(
                !original_owner.is_empty()
                    && original_owner.len() <= 1024
                    && original_owner
                        .chars()
                        .all(|c| c != '/' && c != '\\' && c != '\0')
                    && *full_local <= 0xffffff,
                "source query requires a bounded original basename/full local"
            );
            let owner = crate::esm::load_order::normalize_plugin_name(original_owner);
            let legacy_owner = legacy_owner_key(&owner, &sources);
            (
                format!(
                    "{select} WHERE original_owner IN (?1,?3) AND full_local=?2 AND source_identity_state='resolved' ORDER BY plugin_id,source_record_offset LIMIT {}",
                    QUERY_VERSION_BOUND + 1
                ),
                vec![
                    owner.into(),
                    i64::from(*full_local).into(),
                    legacy_owner.into(),
                ],
            )
        }
    };
    let mut statement = conn.prepare(&sql)?;
    let mut rows = statement.query(rusqlite::params_from_iter(values))?;
    while let Some(row) = rows.next()? {
        summary.versions += 1;
        ensure!(
            summary.versions <= QUERY_VERSION_BOUND,
            "source history query exceeds bounded version count"
        );
        let plugin: u32 = row.get(1)?;
        let source_form_id: u32 = row.get(2)?;
        let record_type: String = row.get(3)?;
        let signature: [u8; 4] = row
            .get::<_, Vec<u8>>(18)?
            .try_into()
            .map_err(|_| color_eyre::eyre::eyre!("invalid stored signature"))?;
        ensure!(
            (plugin as usize) < order.names.len(),
            "absent stored supplying plugin"
        );
        let identity =
            source_identity::resolve(&signature, source_form_id, plugin as usize, &order);
        let owner: Option<String> = row.get(13)?;
        let local: Option<u32> = row.get(14)?;
        let state: String = row.get(15)?;
        ensure!(
            owner
                .as_deref()
                .map(crate::esm::load_order::normalize_plugin_name)
                == identity.original_owner
                && local == identity.full_local
                && state == identity.state.as_str(),
            "stored source identity differs from master authority"
        );
        let offset: u64 = row.get(5)?;
        let candidate = (plugin, offset);
        let source_latest = latest(&conn, &identity, &sources, false)?;
        let admitted_latest = latest(&conn, &identity, &sources, true)?;
        let mut version = SourceVersion {
            runtime_form_id: row.get(0)?,
            plugin_id: plugin,
            supplying_plugin: order.names[plugin as usize].clone(),
            source_form_id,
            record_type,
            record_signature: signature,
            flags: row.get(4)?,
            source_record_offset: offset,
            source_record_size: row.get(6)?,
            source_group_end: row.get(16)?,
            frame_complete: row.get(17)?,
            identity,
            status: row.get(7)?,
            rejection_reason: row.get(8)?,
            source_cell_form_id: row.get(9)?,
            source_worldspace_form_id: row.get(10)?,
            source_topic_form_id: row.get(11)?,
            topic_group_offset: row.get(12)?,
            latest_source_candidate: source_latest == Some(candidate),
            latest_reader_admitted_candidate: admitted_latest == Some(candidate),
            accepted_source_winner: false,
            runtime_record_present: false,
            runtime_fallback_before_latest_source: false,
            raw: None,
            raw_access_failure: None,
        };
        if let Some(id) = version
            .runtime_form_id
            .filter(|_| version.status == "accepted")
        {
            version.accepted_source_winner = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM inhouse_source_records WHERE form_id=?1 AND load_order=?2 AND source_form_id=?3 AND record_type=?4 AND flags=?5)",
                params![id, plugin, source_form_id, version.record_type, version.flags], |r| r.get(0))?;
            version.runtime_record_present = version.accepted_source_winner
                && conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM records WHERE form_id=?1)",
                    [id],
                    |r| r.get(0),
                )?;
            version.runtime_fallback_before_latest_source = version.runtime_record_present
                && source_latest.is_some()
                && source_latest != Some(candidate);
        }
        let (source_name, stored_path, checksum) = &sources[plugin as usize];
        let path: PathBuf = data
            // Preserve the authenticated original filename spelling on case-sensitive hosts.
            .map(|root| root.join(source_name))
            .unwrap_or_else(|| stored_path.clone());
        match guarded_raw(&path, checksum, &version) {
            Ok(raw) => {
                if raw.stored_payload.is_some() {
                    summary.raw_complete += 1;
                } else {
                    summary.header_only += 1;
                }
                version.raw = Some(raw);
            }
            Err(error) => {
                summary.unavailable += 1;
                version.raw_access_failure = Some(error.to_string());
            }
        }
        receive(version)?;
    }
    summary.fully_raw_available =
        summary.versions > 0 && summary.unavailable == 0 && summary.header_only == 0;
    Ok(summary)
}
