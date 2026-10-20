//! Optional tooling provenance: header-only chains and lazy source-version decoding.
use super::{DecodedRecord, PluginDiagnostics, Value, addons::RuntimeSchemas, decoder, scanner};
use crate::esm::{binary::PluginMetadata, load_order::LoadOrder};
use color_eyre::{Result, eyre::ensure};
use rusqlite::{Connection, OpenFlags, Statement, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

/// Stored filename, original source path and native SHA-256.
pub(crate) type PluginSource = (String, PathBuf, Vec<u8>);

/// One physical native record header; rejected versions never participate in winner selection.
#[derive(Clone, Debug)]
pub struct HistoryEntry {
    pub form_id: Option<u32>,
    pub plugin_index: u32,
    pub source_form_id: u32,
    pub record_type: [u8; 4],
    pub flags: u32,
    pub source_record_offset: u64,
    /// Declared on-disk payload bytes, excluding the 24-byte main-record header.
    pub source_record_size: u32,
    pub source_group_end: u64,
    pub frame_complete: bool,
    pub accepted: bool,
    pub reason: Option<String>,
    pub source_cell_form_id: Option<u32>,
    pub source_worldspace_form_id: Option<u32>,
    pub source_topic_form_id: Option<u32>,
    pub topic_group_offset: Option<u64>,
    /// Temporary decoded identity only; no losing payload or typed fields are retained.
    pub editor_id: Option<String>,
}

/// Map header identities using the existing slot/master facts, including undecodable payloads.
pub(crate) fn header_entry(
    header: scanner::ObservedHeader,
    order: &LoadOrder,
    index: usize,
) -> HistoryEntry {
    let form_id = super::source_identity::header_runtime_id(
        &header.record_type,
        header.source_form_id,
        index,
        order,
    );
    HistoryEntry {
        form_id,
        plugin_index: index as u32,
        source_form_id: header.source_form_id,
        record_type: header.record_type,
        flags: header.flags,
        source_record_offset: header.source_record_offset,
        source_record_size: header.source_record_size,
        source_group_end: header.source_group_end,
        frame_complete: header.frame_complete,
        accepted: false,
        // Decoder/candidate rejection supplies its reason only when it occurs.
        reason: header.failure,
        source_cell_form_id: header.cell_form_id,
        source_worldspace_form_id: header.worldspace_form_id,
        source_topic_form_id: header.source_topic_form_id,
        topic_group_offset: header.topic_group_offset,
        editor_id: None,
    }
}

/// Associate rejected GMST identities only through an already decoded EditorID.
pub(crate) fn reconcile_gmst(entries: &mut [HistoryEntry], ids: &HashMap<String, u32>) {
    for entry in entries
        .iter_mut()
        .filter(|entry| !entry.accepted && entry.record_type == *b"GMST")
    {
        if let Some(canonical) = entry
            .editor_id
            .as_ref()
            .and_then(|name| ids.get(name))
            .copied()
        {
            entry.form_id = Some(canonical);
        }
    }
}

/// Persist all headers, including singletons, in a deterministic primary-key tree.
/// The caller supplies its provenance transaction; database failures remain infrastructure errors.
#[cfg(test)]
pub(crate) fn write_tables(
    conn: &Connection,
    entries: &[HistoryEntry],
    order: &LoadOrder,
    paths: &[PathBuf],
) -> Result<()> {
    let schemas = RuntimeSchemas::from_addon_bytes(&[]).map_err(color_eyre::eyre::Error::msg)?;
    write_tables_with_schemas(conn, entries, order, paths, &schemas)
}

/// Atomically retain the exact ordered add-ons beside their producing history snapshot.
/// Configuration is already validated before conversion; this persists reproducible bytes.
pub(crate) fn write_tables_with_schemas(
    conn: &Connection,
    entries: &[HistoryEntry],
    order: &LoadOrder,
    paths: &[PathBuf],
    schemas: &RuntimeSchemas,
) -> Result<()> {
    // Complete conversion replaces this owned tooling snapshot atomically with provenance.
    // Recreate the owned tables so retained history and add-on bytes cannot become stale.
    conn.execute_batch("DROP TABLE IF EXISTS inhouse_schema_addons;
        CREATE TABLE inhouse_schema_addons(
        ordinal INTEGER PRIMARY KEY, name TEXT NOT NULL, sha256 TEXT NOT NULL,
        schema_json BLOB NOT NULL);
        DROP TABLE IF EXISTS record_overrides;
        CREATE TABLE record_overrides(
        form_id INTEGER, chain_position INTEGER NOT NULL,
        plugin_id INTEGER NOT NULL, source_form_id INTEGER NOT NULL,
        deleted INTEGER NOT NULL CHECK(deleted IN (0,1)), record_type TEXT NOT NULL,
        flags INTEGER NOT NULL, source_record_offset INTEGER NOT NULL, source_record_size INTEGER NOT NULL,
        status TEXT NOT NULL CHECK(status IN ('accepted','skipped','header')), reason TEXT,
        source_cell_form_id INTEGER, source_worldspace_form_id INTEGER,
        source_topic_form_id INTEGER, topic_group_offset INTEGER,
        source_path TEXT, schema_sha256 TEXT,
        original_owner TEXT, full_local INTEGER, source_identity_state TEXT NOT NULL,
        source_group_end INTEGER NOT NULL, frame_complete INTEGER NOT NULL CHECK(frame_complete IN (0,1)),
        record_signature BLOB NOT NULL CHECK(length(record_signature)=4),
        PRIMARY KEY(plugin_id,source_record_offset)) WITHOUT ROWID;
        CREATE INDEX record_overrides_chain ON record_overrides(form_id,chain_position);
        CREATE INDEX record_overrides_source ON record_overrides(original_owner,full_local,plugin_id,source_record_offset);")?;
    write_master_table(conn, order)?;
    let mut addon_insert = conn.prepare(
        "INSERT INTO inhouse_schema_addons(ordinal,name,sha256,schema_json) VALUES(?1,?2,?3,?4)",
    )?;
    for (ordinal, (identity, bytes)) in schemas
        .identities()
        .iter()
        .zip(schemas.addon_bytes())
        .enumerate()
    {
        addon_insert.execute(params![
            ordinal as u64,
            identity.name,
            identity.sha256,
            bytes
        ])?;
    }
    let source_paths = paths
        .iter()
        .map(|p| p.canonicalize())
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut sorted: Vec<_> = (0..entries.len()).collect();
    sorted.sort_by_key(|&index| {
        let entry = &entries[index];
        (
            entry.form_id,
            entry.plugin_index,
            entry.source_record_offset,
        )
    });
    let mut chain_positions = vec![0u32; entries.len()];
    let mut previous = None;
    let mut position = 0u32;
    for &index in &sorted {
        let entry = &entries[index];
        let key = entry.form_id;
        if key != previous {
            position = 0;
            previous = key;
        }
        chain_positions[index] = position;
        position += 1;
    }
    // Preserve runtime-chain numbering while writing in the owned table's physical primary key.
    sorted.sort_unstable_by_key(|&index| {
        let entry = &entries[index];
        (entry.plugin_index, entry.source_record_offset)
    });
    let mut insert = conn.prepare("INSERT INTO record_overrides VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23)")?;
    for index in sorted {
        let entry = &entries[index];
        let key = entry.form_id;
        // Unmappable headers retain NULL plus native identity and physical offset.
        let is_header = entry.record_type == *b"TES4";
        let identity = super::source_identity::resolve(
            &entry.record_type,
            entry.source_form_id,
            entry.plugin_index as usize,
            order,
        );
        insert.execute(params![
            key,
            chain_positions[index],
            entry.plugin_index,
            entry.source_form_id,
            entry.flags & 0x20 != 0,
            String::from_utf8_lossy(&entry.record_type),
            entry.flags,
            entry.source_record_offset,
            entry.source_record_size,
            if is_header {
                "header"
            } else if entry.accepted {
                "accepted"
            } else {
                "skipped"
            },
            entry.reason,
            entry.source_cell_form_id,
            entry.source_worldspace_form_id,
            entry.source_topic_form_id,
            entry.topic_group_offset,
            is_header.then(|| source_paths[entry.plugin_index as usize].to_string_lossy()),
            is_header.then(|| schemas.schema_sha256()),
            identity.original_owner,
            identity.full_local,
            identity.state.as_str(),
            entry.source_group_end,
            entry.frame_complete,
            entry.record_type.as_slice()
        ])?;
    }
    Ok(())
}

/// Preserve native MAST order as scalar rows; a zero-master plugin has only its TES4 row.
fn write_master_table(conn: &Connection, order: &LoadOrder) -> Result<()> {
    conn.execute_batch(
        "DROP TABLE IF EXISTS inhouse_plugin_masters;
        CREATE TABLE inhouse_plugin_masters(
            plugin_id INTEGER NOT NULL, master_index INTEGER NOT NULL CHECK(master_index>=0),
            master_name TEXT NOT NULL,
            PRIMARY KEY(plugin_id,master_index)) WITHOUT ROWID;",
    )?;
    let mut insert = conn.prepare("INSERT INTO inhouse_plugin_masters VALUES(?1,?2,?3)")?;
    for (plugin_id, metadata) in order.metadata.iter().enumerate() {
        for (master_index, master) in metadata.masters.iter().enumerate() {
            insert.execute(params![
                i64::try_from(plugin_id)?,
                i64::try_from(master_index)?,
                master
            ])?;
        }
    }
    Ok(())
}

/// Header/source facts and the independently decoded result for one version.
#[derive(Debug, Serialize)]
pub struct HistoryVersion {
    pub chain_position: u32,
    pub plugin: String,
    pub plugin_id: u32,
    pub source_form_id: u32,
    pub deleted: bool,
    pub source_record_offset: u64,
    pub source_record_size: u32,
    pub status: String,
    pub reason: Option<String>,
    pub winner: bool,
    pub decode_status: String,
    pub decode_reason: Option<String>,
    pub record: Option<DecodedRecord>,
    pub diagnostics: PluginDiagnostics,
}

/// Requested chain plus a field-occurrence comparison, suitable for scripts and text views.
#[derive(Debug, Serialize)]
pub struct HistoryReport {
    pub form_id: u32,
    pub versions: Vec<HistoryVersion>,
    pub fields: Vec<HistoryField>,
}

/// Compare a stable authored role and occurrence across the requested source versions.
#[derive(Debug, Serialize)]
pub struct HistoryField {
    pub field: String,
    pub different: bool,
    pub values: Vec<Option<Value>>,
}

/// Reconstitute original slot/master assignment without reading any source plugin headers.
/// New packs use scalar ordered master rows; retained older packs keep read compatibility.
pub(crate) fn stored_order(conn: &Connection) -> Result<(LoadOrder, Vec<PluginSource>)> {
    let normalized: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='inhouse_plugin_masters')",
        [], |r| r.get(0),
    )?;
    let legacy_column = if normalized {
        "NULL"
    } else {
        "h.plugin_masters"
    };
    let mut statement = conn.prepare(&format!("SELECT h.plugin_id,p.name,h.flags,h.source_path,p.checksum,{legacy_column} FROM plugins p JOIN record_overrides h ON h.plugin_id=p.id AND h.record_type='TES4' AND h.source_record_offset=0 ORDER BY p.priority,p.id"))?;
    let mut master_statement = normalized.then(|| conn.prepare(
        "SELECT master_index,master_name FROM inhouse_plugin_masters WHERE plugin_id=?1 ORDER BY master_index"
    )).transpose()?;
    let rows = statement.query_map([], |r| {
        Ok((
            r.get::<_, u32>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, u32>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Vec<u8>>(4)?,
            r.get::<_, Option<String>>(5)?,
        ))
    })?;
    let mut names = Vec::new();
    let mut metadata = Vec::new();
    let mut sources = Vec::new();
    for row in rows {
        let (plugin_id, name, flags, source, checksum, legacy_masters) = row?;
        let masters = if let Some(statement) = &mut master_statement {
            read_normalized_masters(statement, plugin_id)?
        } else {
            let legacy_masters = legacy_masters.ok_or_else(|| {
                color_eyre::eyre::eyre!("plugin {plugin_id}: missing stored legacy master list")
            })?;
            serde_json::from_str(&legacy_masters)?
        };
        names.push(name.clone());
        metadata.push(PluginMetadata { flags, masters });
        sources.push((name, source.into(), checksum));
    }
    Ok((LoadOrder::from_metadata(names, metadata)?, sources))
}

/// Reject ambiguous edited SQL ordinals before rebuilding file-relative master indices.
fn read_normalized_masters(statement: &mut Statement<'_>, plugin_id: u32) -> Result<Vec<String>> {
    let mut rows = statement.query([plugin_id])?;
    let mut masters = Vec::new();
    while let Some(row) = rows.next()? {
        let master_index: i64 = row.get(0)?;
        ensure!(
            master_index == i64::try_from(masters.len())?,
            "plugin {plugin_id}: stored master ordinal {master_index} is not contiguous; expected {}",
            masters.len()
        );
        masters.push(row.get(1)?);
    }
    Ok(masters)
}

/// Hash a source handle, then read only the requested native record with bounded allocation.
fn decode_version(
    path: &Path,
    expected: &[u8],
    entry: &HistoryEntry,
    order: &LoadOrder,
    kinds: &HashMap<u32, [u8; 4]>,
    runtime: (&RuntimeSchemas, &decoder::PreparedSchemas),
    diagnostics: &mut PluginDiagnostics,
) -> Result<DecodedRecord> {
    let (schemas, prepared) = runtime;
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    ensure!(
        hasher.finalize().as_slice() == expected,
        "plugin changed: checksum mismatch; version not decoded"
    );
    file.seek(SeekFrom::Start(entry.source_record_offset))?;
    let mut header = [0u8; 24];
    file.read_exact(&mut header)?;
    let word = |offset| {
        u32::from_le_bytes(
            header[offset..offset + 4]
                .try_into()
                .expect("fixed native header"),
        )
    };
    ensure!(
        header[..4] == entry.record_type
            && word(4) == entry.source_record_size
            && word(8) == entry.flags
            && word(12) == entry.source_form_id,
        "stored record header does not match source"
    );
    let length = file.metadata()?.len();
    ensure!(
        entry.frame_complete
            && entry.source_group_end <= length
            && entry.source_record_offset.saturating_add(24) <= entry.source_group_end
            && u64::from(entry.source_record_size)
                <= entry
                    .source_group_end
                    .saturating_sub(entry.source_record_offset.saturating_add(24)),
        "truncated native record payload"
    );
    ensure!(
        entry.source_record_size <= 64 * 1024 * 1024,
        "record payload exceeds diagnostic allocation bound"
    );
    let mut payload = vec![0; entry.source_record_size as usize];
    file.read_exact(&mut payload)?;
    if entry.flags & 0x40000 != 0 {
        payload = scanner::inflate(&payload).map_err(color_eyre::eyre::Error::msg)?;
    }
    let scanned = scanner::ScannedRecord {
        record_type: entry.record_type,
        flags: entry.flags,
        source_form_id: entry.source_form_id,
        version_control: word(16),
        form_version: u16::from_le_bytes(header[20..22].try_into().expect("fixed native header")),
        header_unknown: u16::from_le_bytes(header[22..24].try_into().expect("fixed native header")),
        cell_form_id: entry.source_cell_form_id,
        worldspace_form_id: entry.source_worldspace_form_id,
        source_topic_form_id: entry.source_topic_form_id,
        topic_group_offset: entry.topic_group_offset,
        source_record_offset: entry.source_record_offset,
        payload,
    };
    let context = decoder::Context {
        order,
        plugin_index: entry.plugin_index as usize,
        localized: order.metadata[entry.plugin_index as usize].flags & 0x80 != 0,
        winning_types: Some(kinds),
    };
    let mut record =
        decoder::decode_with_prepared(scanned, schemas.schema(), prepared, &context, diagnostics)
            .map_err(color_eyre::eyre::Error::msg)?;
    record.form_id = entry.form_id.unwrap_or(record.form_id);
    ensure!(
        record.payload_complete && record.rejected_fields.is_empty(),
        "undecodable fields or incomplete subrecord framing: {:?}",
        record.rejected_fields
    );
    Ok(record)
}

/// Reopen saved add-on bytes without depending on files from the converter's invocation.
/// Older packs have no add-on table and retain the embedded-schema compatibility check.
pub(crate) fn stored_schemas(conn: &Connection) -> Result<RuntimeSchemas> {
    let has_addons: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='inhouse_schema_addons')",
        [],
        |row| row.get(0),
    )?;
    let mut bytes = Vec::new();
    let mut identities = Vec::new();
    let mut total_bytes = 0usize;
    if has_addons {
        let mut statement = conn.prepare(
            "SELECT ordinal,name,sha256,schema_json FROM inhouse_schema_addons ORDER BY ordinal",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let ordinal = row.get::<_, i64>(0)?;
            let name = row.get::<_, String>(1)?;
            let sha256 = row.get::<_, String>(2)?;
            let schema_json = row.get_ref(3)?.as_blob()?;
            ensure!(
                bytes.len() < super::addons::MAX_SCHEMA_ADDONS
                    && schema_json.len() <= super::addons::MAX_SCHEMA_ADDON_BYTES,
                "stored schema add-ons exceed their count or document-size limit"
            );
            total_bytes = total_bytes.saturating_add(schema_json.len());
            ensure!(
                total_bytes <= super::addons::MAX_SCHEMA_ADDON_TOTAL_BYTES,
                "stored schema add-ons exceed their total-size limit"
            );
            ensure!(
                usize::try_from(ordinal).ok() == Some(bytes.len()),
                "stored schema add-on order is not contiguous"
            );
            ensure!(
                crate::cache::hash_bytes(schema_json) == sha256,
                "stored schema add-on {name} checksum mismatch"
            );
            bytes.push(schema_json.to_vec());
            identities.push((name, sha256));
        }
    }
    let references = bytes.iter().map(Vec::as_slice).collect::<Vec<_>>();
    let schemas =
        RuntimeSchemas::from_addon_bytes(&references).map_err(color_eyre::eyre::Error::msg)?;
    ensure!(
        schemas.identities().len() == identities.len()
            && schemas
                .identities()
                .iter()
                .zip(&identities)
                .all(|(identity, (name, sha256))| identity.name == *name
                    && identity.sha256 == *sha256),
        "stored schema add-on identities differ from their saved definitions"
    );
    let mut statement = conn
        .prepare("SELECT DISTINCT schema_sha256 FROM record_overrides WHERE record_type='TES4'")?;
    let fingerprints = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        fingerprints.len() == 1 && fingerprints[0] == schemas.schema_sha256(),
        "diagnostic schema differs from the producing schema"
    );
    Ok(schemas)
}

/// Load a requested chain read-only; one missing, changed or undecodable source remains local.
pub fn read_history(pack: &Path, data: Option<&Path>, form_id: u32) -> Result<HistoryReport> {
    let database = if pack.is_file() {
        pack.to_path_buf()
    } else {
        pack.join("skyrim_world.db")
    };
    let conn = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let (order, sources) = stored_order(&conn)?;
    // Schema/provenance failures leave the requested history visible, but forbid decoding
    // with a different schema or an add-on set that only partly passed validation.
    let schemas = stored_schemas(&conn).map_err(|error| format!("{error}"));
    let prepared = schemas.as_ref().ok().map(decoder::prepare_runtime_schema);
    let mut kind_statement =
        conn.prepare("SELECT form_id,record_type FROM inhouse_source_records")?;
    let kinds = kind_statement
        .query_map([], |r| Ok((r.get::<_, u32>(0)?, r.get::<_, String>(1)?)))?
        .filter_map(|row| row.ok())
        .filter_map(|(id, tag)| tag.as_bytes().try_into().ok().map(|tag| (id, tag)))
        .collect();
    // The retained 18-column layout predates authenticated group bounds and raw signatures.
    // Keep its available chain facts visible, but never infer missing replay authority.
    let mut layout = conn.prepare("PRAGMA table_info(record_overrides)")?;
    let columns = layout
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let retained_column = |name: &str| {
        if columns.iter().any(|column| column == name) {
            name.to_owned()
        } else {
            "NULL".to_owned()
        }
    };
    let mut statement = conn.prepare(&format!(
        "SELECT chain_position,plugin_id,source_form_id,record_type,flags,source_record_offset,source_record_size,status,reason,source_cell_form_id,source_worldspace_form_id,source_topic_form_id,topic_group_offset,{},{},{} FROM record_overrides WHERE form_id=?1 ORDER BY chain_position",
        retained_column("source_group_end"),
        retained_column("frame_complete"),
        retained_column("record_signature")
    ))?;
    let rows = statement.query_map([form_id], |r| {
        Ok((
            r.get::<_, u32>(0)?,
            r.get::<_, u32>(1)?,
            r.get::<_, u32>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, u32>(4)?,
            r.get::<_, u64>(5)?,
            r.get::<_, u32>(6)?,
            r.get::<_, String>(7)?,
            r.get::<_, Option<String>>(8)?,
            r.get::<_, Option<u32>>(9)?,
            r.get::<_, Option<u32>>(10)?,
            r.get::<_, Option<u32>>(11)?,
            r.get::<_, Option<u64>>(12)?,
            r.get::<_, Option<u64>>(13)?,
            r.get::<_, Option<bool>>(14)?,
            r.get::<_, Option<Vec<u8>>>(15)?,
        ))
    })?;
    let mut versions = Vec::new();
    for row in rows {
        let (
            position,
            index,
            source,
            _tag,
            flags,
            offset,
            size,
            status,
            reason,
            cell,
            world,
            topic,
            group,
            containing_group_end,
            frame_complete,
            signature,
        ) = row?;
        let mut version = HistoryVersion {
            chain_position: position,
            plugin: sources
                .get(index as usize)
                .map(|s| s.0.clone())
                .unwrap_or_else(|| format!("plugin {index}")),
            plugin_id: index,
            source_form_id: source,
            deleted: flags & 0x20 != 0,
            source_record_offset: offset,
            source_record_size: size,
            status: status.clone(),
            reason: reason.clone(),
            winner: false,
            decode_status: "undecodable".into(),
            decode_reason: None,
            record: None,
            diagnostics: PluginDiagnostics::default(),
        };
        let decoded = (|| -> Result<DecodedRecord> {
            let containing_group_end = containing_group_end.ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "stored history lacks authenticated containing-group bounds"
                )
            })?;
            let frame_complete = frame_complete.ok_or_else(|| {
                color_eyre::eyre::eyre!("stored history lacks authenticated frame completeness")
            })?;
            let signature = signature.ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "stored history lacks an authenticated raw record signature"
                )
            })?;
            let schemas = schemas
                .as_ref()
                .map_err(|error| color_eyre::eyre::eyre!("{error}"))?;
            let prepared = prepared.as_ref().expect("schema prepared after validation");
            let (name, path, checksum) = sources
                .get(index as usize)
                .ok_or_else(|| color_eyre::eyre::eyre!("absent stored source metadata"))?;
            let path = data
                .map(|root| root.join(name))
                .unwrap_or_else(|| path.clone());
            let entry = HistoryEntry {
                form_id: Some(form_id),
                plugin_index: index,
                source_form_id: source,
                record_type: signature
                    .as_slice()
                    .try_into()
                    .map_err(|_| color_eyre::eyre::eyre!("invalid stored signature"))?,
                flags,
                source_record_offset: offset,
                source_record_size: size,
                source_group_end: containing_group_end,
                frame_complete,
                accepted: status == "accepted",
                reason,
                source_cell_form_id: cell,
                source_worldspace_form_id: world,
                source_topic_form_id: topic,
                topic_group_offset: group,
                editor_id: None,
            };
            decode_version(
                &path,
                checksum,
                &entry,
                &order,
                &kinds,
                (schemas, prepared),
                &mut version.diagnostics,
            )
        })();
        match decoded {
            Ok(record) => {
                version.decode_status = "decoded".into();
                version.record = Some(record);
            }
            Err(error) => version.decode_reason = Some(format!("{error}")),
        }
        versions.push(version);
    }
    if let Some(last) = versions.iter_mut().rev().find(|v| v.status == "accepted") {
        last.winner = !last.deleted;
    }
    let mut roles = BTreeMap::<String, Vec<Option<Value>>>::new();
    for (column, version) in versions.iter().enumerate() {
        let mut occurrences = HashMap::<String, u32>::new();
        if let Some(record) = &version.record {
            for field in &record.fields {
                let count = occurrences.entry(field.name.clone()).or_default();
                let name = format!(
                    "{}:{}[{}]",
                    String::from_utf8_lossy(&field.signature),
                    field.name,
                    *count
                );
                *count += 1;
                roles
                    .entry(name)
                    .or_insert_with(|| vec![None; versions.len()])[column] =
                    Some(field.value.clone());
            }
        }
    }
    let fields = roles
        .into_iter()
        .map(|(field, values)| HistoryField {
            different: values.windows(2).any(|v| v[0] != v[1]),
            field,
            values,
        })
        .collect();
    Ok(HistoryReport {
        form_id,
        versions,
        fields,
    })
}

/// Print load-order columns and mark every changed or absent authored field occurrence.
pub fn display(report: &HistoryReport) -> String {
    let mut output = format!("Record {:08X}\n", report.form_id);
    for version in &report.versions {
        output.push_str(&format!(
            "{}: {} source={:08X} offset={} size={} {}{}{} {}{}\n",
            version.chain_position,
            version.plugin,
            version.source_form_id,
            version.source_record_offset,
            version.source_record_size,
            version.status,
            if version.deleted { " deleted" } else { "" },
            if version.winner { " winner" } else { "" },
            version.decode_status,
            version
                .decode_reason
                .as_ref()
                .map(|s| format!(": {s}"))
                .unwrap_or_default()
        ));
    }
    output.push_str("Field | ");
    output.push_str(
        &report
            .versions
            .iter()
            .map(|v| v.plugin.as_str())
            .collect::<Vec<_>>()
            .join(" | "),
    );
    output.push('\n');
    for field in &report.fields {
        output.push_str(&format!(
            "{}{} | {}\n",
            if field.different { "* " } else { "  " },
            field.field,
            field
                .values
                .iter()
                .map(|v| v
                    .as_ref()
                    .map(|v| serde_json::to_string(v)
                        .unwrap_or_else(|_| "unrepresentable value".into()))
                    .unwrap_or_else(|| "<absent>".into()))
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    output
}

#[cfg(test)]
mod normalized_masters_tests {
    use super::*;

    /// Use a different native master order from global full/light slot order.
    fn fixture_order() -> LoadOrder {
        LoadOrder::from_metadata(
            ["Base.esm", "Second.esm", "Light.esl", "Patch.esp"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            vec![
                PluginMetadata {
                    flags: 0,
                    masters: vec![],
                },
                PluginMetadata {
                    flags: 0,
                    masters: vec![],
                },
                PluginMetadata {
                    flags: 0x200,
                    masters: vec![],
                },
                PluginMetadata {
                    flags: 0,
                    masters: vec!["Light.esl".into(), "Base.esm".into(), "Second.esm".into()],
                },
            ],
        )
        .unwrap()
    }

    /// Build only stored SQL metadata; historical fixtures can include their old JSON column.
    fn metadata_database(legacy: bool) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        let legacy_column = if legacy { ",plugin_masters TEXT" } else { "" };
        conn.execute_batch(&format!(
            "CREATE TABLE plugins(
            id INTEGER PRIMARY KEY,name TEXT,priority INTEGER,checksum BLOB);
            CREATE TABLE record_overrides(
                plugin_id INTEGER,record_type TEXT,source_record_offset INTEGER,
                flags INTEGER,source_path TEXT{legacy_column});"
        ))
        .unwrap();
        let order = fixture_order();
        for (plugin_id, name) in order.names.iter().enumerate() {
            conn.execute(
                "INSERT INTO plugins VALUES(?1,?2,?1,?3)",
                params![plugin_id as u32, name, vec![0x39u8; 32]],
            )
            .unwrap();
            conn.execute("INSERT INTO record_overrides(plugin_id,record_type,source_record_offset,flags,source_path) VALUES(?1,'TES4',0,?2,?3)",
                params![plugin_id as u32, order.metadata[plugin_id].flags, name]).unwrap();
        }
        if legacy {
            // This intentionally models the retained old-pack storage format only.
            conn.execute_batch("UPDATE record_overrides SET plugin_masters='[]';
                UPDATE record_overrides SET plugin_masters='[\"Light.esl\",\"Base.esm\",\"Second.esm\"]' WHERE plugin_id=3;")
                .unwrap();
        }
        conn
    }

    /// Compare owner mapping independently of the SQL insertion or reader iteration order.
    fn assert_authority(order: &LoadOrder, sources: &[PluginSource]) {
        assert_eq!(
            order.names,
            ["base.esm", "second.esm", "light.esl", "patch.esp"]
        );
        assert!(
            order.metadata[..3]
                .iter()
                .all(|header| header.masters.is_empty())
        );
        assert_eq!(
            order.metadata[3].masters,
            ["Light.esl", "Base.esm", "Second.esm"]
        );
        assert_eq!(sources.len(), 4);
        assert_eq!(
            super::super::source_identity::header_runtime_id(b"STAT", 0x00000abc, 3, order),
            Some(0xfe000abc)
        );
        assert_eq!(
            super::super::source_identity::header_runtime_id(b"STAT", 0x01000123, 3, order),
            Some(0x00000123)
        );
        assert_eq!(
            super::super::source_identity::header_runtime_id(b"STAT", 0x02000678, 3, order),
            Some(0x01000678)
        );
    }

    /// New history writes contain scalar masters, no JSON column, and explicit zero-master headers.
    #[test]
    fn normalized_producer_roundtrips_native_order_and_zero_masters() {
        let conn = metadata_database(false);
        let order = fixture_order();
        let temp = tempfile::tempdir().unwrap();
        let paths: Vec<_> = order
            .names
            .iter()
            .map(|name| {
                let path = temp.path().join(name);
                std::fs::write(&path, []).unwrap();
                path
            })
            .collect();
        let entries: Vec<_> = (0..order.names.len())
            .map(|index| {
                header_entry(
                    scanner::ObservedHeader {
                        record_type: *b"TES4",
                        source_form_id: 0,
                        flags: order.metadata[index].flags,
                        source_record_offset: 0,
                        source_record_size: 0,
                        source_group_end: 24,
                        frame_complete: true,
                        cell_form_id: None,
                        worldspace_form_id: None,
                        source_topic_form_id: None,
                        topic_group_offset: None,
                        failure: None,
                    },
                    &order,
                    index,
                )
            })
            .collect();
        write_tables(&conn, &entries, &order, &paths).unwrap();
        let json_columns: i64 = conn.query_row("SELECT count(*) FROM pragma_table_info('record_overrides') WHERE name='plugin_masters'", [], |row| row.get(0)).unwrap();
        let zero_master_headers: i64 = conn.query_row("SELECT count(*) FROM record_overrides h WHERE h.record_type='TES4' AND NOT EXISTS(SELECT 1 FROM inhouse_plugin_masters m WHERE m.plugin_id=h.plugin_id)", [], |row| row.get(0)).unwrap();
        assert_eq!((json_columns, zero_master_headers), (0, 3));
        let rows = conn.prepare("SELECT plugin_id,master_index,master_name FROM inhouse_plugin_masters ORDER BY plugin_id,master_index").unwrap()
            .query_map([], |row| Ok((row.get::<_,u32>(0)?,row.get::<_,u32>(1)?,row.get::<_,String>(2)?))).unwrap()
            .collect::<rusqlite::Result<Vec<_>>>().unwrap();
        assert_eq!(
            rows,
            [
                (3, 0, "Light.esl".into()),
                (3, 1, "Base.esm".into()),
                (3, 2, "Second.esm".into())
            ]
        );
        let (stored, sources) = stored_order(&conn).unwrap();
        assert_authority(&stored, &sources);
    }

    /// Normalized ordinals determine authority even when scalar rows were inserted in reverse order.
    #[test]
    fn normalized_reader_uses_ordinals_and_ignores_stale_legacy_json() {
        let conn = metadata_database(true);
        write_master_table(&conn, &fixture_order()).unwrap();
        conn.execute_batch("DELETE FROM inhouse_plugin_masters;
            INSERT INTO inhouse_plugin_masters VALUES(3,2,'Second.esm'),(3,1,'Base.esm'),(3,0,'Light.esl');
            UPDATE record_overrides SET plugin_masters='not JSON';")
            .unwrap();
        let (order, sources) = stored_order(&conn).unwrap();
        assert_authority(&order, &sources);
    }

    /// A missing first or interior ordinal is an explicit stored-metadata failure without fallback.
    #[test]
    fn normalized_reader_rejects_sparse_ordinals() {
        for removed in [0u32, 1] {
            let conn = metadata_database(true);
            write_master_table(&conn, &fixture_order()).unwrap();
            conn.execute(
                "DELETE FROM inhouse_plugin_masters WHERE plugin_id=3 AND master_index=?1",
                [removed],
            )
            .unwrap();
            let error = stored_order(&conn).err().unwrap().to_string();
            assert!(
                error.contains("plugin 3") && error.contains("not contiguous"),
                "{error}"
            );
        }
    }

    /// An edited unconstrained table cannot reinterpret a negative ordinal as a valid index.
    #[test]
    fn normalized_reader_rejects_negative_ordinals() {
        let conn = metadata_database(false);
        conn.execute_batch("CREATE TABLE inhouse_plugin_masters(plugin_id INTEGER,master_index INTEGER,master_name TEXT);
            INSERT INTO inhouse_plugin_masters VALUES(3,-1,'Base.esm');")
            .unwrap();
        let error = stored_order(&conn).err().unwrap().to_string();
        assert!(
            error.contains("ordinal -1") && error.contains("expected 0"),
            "{error}"
        );
    }

    /// Retained packs with only the former JSON column rebuild the same full/light authority.
    #[test]
    fn retained_legacy_pack_remains_readable() {
        let conn = metadata_database(true);
        let (order, sources) = stored_order(&conn).unwrap();
        assert_authority(&order, &sources);
    }
}

#[cfg(test)]
mod schema_addon_controls {
    use super::*;

    const ADDON: &[u8] = br#"{"version":1,"name":"history_control","records":[{"signature":"ZHIS","source":"synthetic history fixture","fields":[{"signature":"ZNAM","name":"neighbor","kind":"u32"}]}]}"#;

    /// Store the public provenance contract independently of the production history writer.
    fn snapshot(schemas: &RuntimeSchemas) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE record_overrides(record_type TEXT,schema_sha256 TEXT);
             CREATE TABLE inhouse_schema_addons(ordinal INTEGER PRIMARY KEY,name TEXT,sha256 TEXT,schema_json BLOB);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO record_overrides VALUES('TES4',?1)",
            [schemas.schema_sha256()],
        )
        .unwrap();
        for (ordinal, (identity, bytes)) in schemas
            .identities()
            .iter()
            .zip(schemas.addon_bytes())
            .enumerate()
        {
            conn.execute(
                "INSERT INTO inhouse_schema_addons VALUES(?1,?2,?3,?4)",
                params![ordinal as u64, identity.name, identity.sha256, bytes],
            )
            .unwrap();
        }
        conn
    }

    /// Exact retained documents restore new types without any original add-on file path.
    #[test]
    fn retained_documents_restore_schema_and_legacy_packs_keep_builtin_hash() {
        let schemas = RuntimeSchemas::from_addon_bytes(&[ADDON]).unwrap();
        let conn = snapshot(&schemas);
        let reopened = stored_schemas(&conn).unwrap();
        assert_eq!(reopened.identities(), schemas.identities());
        assert_eq!(reopened.schema_sha256(), schemas.schema_sha256());
        assert!(
            reopened
                .schema()
                .records
                .iter()
                .any(|record| record.signature == "ZHIS")
        );

        let builtins = RuntimeSchemas::from_addon_bytes(&[]).unwrap();
        let legacy = snapshot(&builtins);
        legacy
            .execute("DROP TABLE inhouse_schema_addons", [])
            .unwrap();
        assert_eq!(
            stored_schemas(&legacy).unwrap().schema_sha256(),
            crate::cache::hash_bytes(super::super::SCHEMA_BYTES)
        );
    }

    /// Corrupt saved metadata never supplies a different or partially validated decoding schema.
    #[test]
    fn stored_metadata_corruption_rejects_order_names_hashes_and_fingerprints() {
        let schemas = RuntimeSchemas::from_addon_bytes(&[ADDON]).unwrap();
        for mutation in [
            "UPDATE inhouse_schema_addons SET ordinal=-1",
            "UPDATE inhouse_schema_addons SET name='other_name'",
            "UPDATE inhouse_schema_addons SET sha256='wrong_hash'",
            "UPDATE inhouse_schema_addons SET schema_json=x'7b7d'",
            "UPDATE record_overrides SET schema_sha256='wrong_fingerprint'",
            "INSERT INTO record_overrides VALUES('TES4','different_header_fingerprint')",
        ] {
            let conn = snapshot(&schemas);
            conn.execute(mutation, []).unwrap();
            assert!(stored_schemas(&conn).is_err(), "accepted {mutation}");
        }
    }

    /// A subsequent no-add-on snapshot clears stale add-on rows in a reused database.
    #[test]
    fn builtin_history_write_clears_previous_addon_provenance() {
        let conn = Connection::open_in_memory().unwrap();
        let order = LoadOrder::from_metadata(Vec::new(), Vec::new()).unwrap();
        let schemas = RuntimeSchemas::from_addon_bytes(&[ADDON]).unwrap();
        write_tables_with_schemas(&conn, &[], &order, &[], &schemas).unwrap();
        let count = |conn: &Connection| {
            conn.query_row("SELECT COUNT(*) FROM inhouse_schema_addons", [], |row| {
                row.get::<_, u64>(0)
            })
            .unwrap()
        };
        assert_eq!(count(&conn), 1);
        write_tables(&conn, &[], &order, &[]).unwrap();
        assert_eq!(count(&conn), 0);
    }
}
