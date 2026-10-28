//! Complete per-record recovery events, spooled to disk with bounded resident state.
//!
//! The existing plugin counters remain suitable for console summaries. This journal
//! records every distinct occurrence and publishes only scalar SQL columns. Journal
//! failures are infrastructure errors: they prevent publication of an incomplete log.
use color_eyre::{Result, eyre::eyre};
use rusqlite::{Connection, OptionalExtension, Row, params};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex, MutexGuard};
use tempfile::TempPath;

/// Tooling schema identity; this does not change the runtime database/cache ABI.
pub(crate) const SCHEMA_VERSION: u32 = 1;
const REASON_BYTES: usize = 2048;
const FIELD_BYTES: usize = 512;

/// Physical record and authored field context at the point an action is taken.
#[derive(Clone, Debug, Default)]
pub struct EventContext {
    pub source_record_offset: Option<u64>,
    pub source_form_id: Option<u32>,
    pub form_id: Option<u32>,
    pub record_type: Option<[u8; 4]>,
    pub stage: &'static str,
    /// A nested member path, including physical field or array indices where known.
    pub field: Option<String>,
    /// Native or resolved link value, according to the named stage.
    pub link_value: Option<u32>,
    /// Distinguishes repeated observations in the same field or array.
    pub occurrence: u64,
    pub action: Option<&'static str>,
}

/// A lazily created private journal. Clones share the same journal, not a snapshot.
#[derive(Clone, Debug, Default)]
pub struct EventLog {
    inner: Arc<Mutex<LogState>>,
}

/// Fixed-size process state; all retained events live in the SQLite spool.
#[derive(Debug, Default)]
struct LogState {
    journal: Option<Journal>,
    failure: Option<String>,
}

/// The connection closes before its temporary path is released, including on Windows.
#[derive(Debug)]
struct Journal {
    conn: Connection,
    _path: TempPath,
}

/// One bounded event row while streaming between journals and the publication table.
struct EventRow {
    key: Vec<u8>,
    source_record_offset: Option<i64>,
    source_form_id: Option<u32>,
    form_id: Option<u32>,
    record_type: Option<Vec<u8>>,
    stage: String,
    field: Option<String>,
    occurrence: i64,
    link_value: Option<u32>,
    category: String,
    action: String,
    reason: String,
    field_truncated: bool,
    reason_truncated: bool,
}

const EVENT_COLUMNS: &str = "event_key,source_record_offset,source_form_id,form_id,record_type,stage,field,occurrence,link_value,category,action,reason,field_truncated,reason_truncated";
const INSERT_EVENT: &str =
    "INSERT INTO events VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)
    ON CONFLICT(event_key) DO UPDATE SET form_id=COALESCE(events.form_id,excluded.form_id)";

impl Journal {
    /// Configure a disk journal with a small cache and disk-backed temporary work.
    fn new() -> Result<Self> {
        let path = tempfile::Builder::new()
            .prefix("mudcrab-record-diagnostics-")
            .tempfile()?
            .into_temp_path();
        let conn = Connection::open(&path)?;
        conn.execute_batch(
            "PRAGMA cache_size=-64;
             PRAGMA temp_store=FILE;
             PRAGMA cache_spill=ON;
             PRAGMA synchronous=OFF;
             CREATE TABLE events(
                event_key BLOB PRIMARY KEY CHECK(length(event_key)=32),
                source_record_offset INTEGER, source_form_id INTEGER, form_id INTEGER,
                record_type BLOB, stage TEXT NOT NULL, field TEXT,
                occurrence INTEGER NOT NULL, link_value INTEGER,
                category TEXT NOT NULL, action TEXT NOT NULL, reason TEXT NOT NULL,
                field_truncated INTEGER NOT NULL, reason_truncated INTEGER NOT NULL
             ) WITHOUT ROWID;
             BEGIN IMMEDIATE;",
        )?;
        // mmap_size returns the resulting limit even when used as a setter.
        conn.query_row("PRAGMA mmap_size=0", [], |row| row.get::<_, i64>(0))?;
        Ok(Self { conn, _path: path })
    }
}

impl EventRow {
    /// Bound displayed text while hashing the complete original event identity.
    fn from_context(context: &EventContext, category: &str, reason: &str) -> Result<Self> {
        let action = context.action.unwrap_or(match category {
            "record" | "field" | "unexpected" => "skipped",
            "compatibility" => "projected",
            _ => "cleared",
        });
        let mut identity = Sha256::new();
        hash_word(&mut identity, context.source_record_offset);
        hash_word(&mut identity, context.source_form_id.map(u64::from));
        hash_bytes(&mut identity, context.record_type.as_ref().map(|v| &v[..]));
        hash_bytes(&mut identity, Some(context.stage.as_bytes()));
        hash_bytes(&mut identity, context.field.as_deref().map(str::as_bytes));
        hash_word(&mut identity, Some(context.occurrence));
        hash_word(&mut identity, context.link_value.map(u64::from));
        hash_bytes(&mut identity, Some(category.as_bytes()));
        hash_bytes(&mut identity, Some(action.as_bytes()));
        hash_bytes(&mut identity, Some(reason.as_bytes()));
        // The resolved runtime ID can be filled during replay or history reconciliation.
        // It is not a second event when the physical source and action are identical.
        let field = context
            .field
            .as_deref()
            .map(|v| bounded_text(v, FIELD_BYTES));
        let (reason, reason_truncated) = bounded_text(reason, REASON_BYTES);
        Ok(Self {
            key: identity.finalize().to_vec(),
            source_record_offset: context
                .source_record_offset
                .map(i64::try_from)
                .transpose()?,
            source_form_id: context.source_form_id,
            form_id: context.form_id,
            record_type: context.record_type.map(|v| v.to_vec()),
            stage: context.stage.into(),
            field_truncated: field.as_ref().is_some_and(|v| v.1),
            field: field.map(|v| v.0),
            occurrence: i64::try_from(context.occurrence)?,
            link_value: context.link_value,
            category: category.into(),
            action: action.into(),
            reason,
            reason_truncated,
        })
    }

    /// Read one row without accumulating any of its neighbors.
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            key: row.get(0)?,
            source_record_offset: row.get(1)?,
            source_form_id: row.get(2)?,
            form_id: row.get(3)?,
            record_type: row.get(4)?,
            stage: row.get(5)?,
            field: row.get(6)?,
            occurrence: row.get(7)?,
            link_value: row.get(8)?,
            category: row.get(9)?,
            action: row.get(10)?,
            reason: row.get(11)?,
            field_truncated: row.get(12)?,
            reason_truncated: row.get(13)?,
        })
    }

    /// Insert or fill the missing runtime identity of an exact replay duplicate.
    fn insert(&self, conn: &Connection) -> rusqlite::Result<()> {
        conn.prepare_cached(INSERT_EVENT)?.execute(params![
            self.key,
            self.source_record_offset,
            self.source_form_id,
            self.form_id,
            self.record_type,
            self.stage,
            self.field,
            self.occurrence,
            self.link_value,
            self.category,
            self.action,
            self.reason,
            self.field_truncated,
            self.reason_truncated,
        ])?;
        Ok(())
    }
}

impl EventLog {
    /// Record every distinct event; failure is retained for the publication boundary.
    pub(crate) fn append(&self, context: &EventContext, category: &str, reason: &str) {
        let mut state = self.lock();
        if state.failure.is_some() {
            return;
        }
        let result = (|| -> Result<()> {
            if state.journal.is_none() {
                state.journal = Some(Journal::new()?);
            }
            let row = EventRow::from_context(context, category, reason)?;
            row.insert(&state.journal.as_ref().expect("initialized journal").conn)?;
            Ok(())
        })();
        if let Err(error) = result {
            state.failure = Some(error.to_string());
        }
    }

    /// Union exact identities, preserving distinct physical fields and occurrences.
    pub(crate) fn merge_from(&self, other: &Self) {
        self.merge(other, None);
    }

    /// Add replay events absent from a separate snapshot of the pre-replay journal.
    pub(crate) fn merge_new_from(&self, resolved: &Self, baseline: &Self) {
        self.merge(resolved, Some(baseline));
    }

    /// Acquire logs in address order so simultaneous opposite-direction merges cannot deadlock.
    fn merge(&self, other: &Self, baseline: Option<&Self>) {
        if Arc::ptr_eq(&self.inner, &other.inner) {
            return;
        }
        // The list contains at most three handles regardless of event count.
        let mut logs = vec![self, other];
        logs.extend(baseline);
        logs.sort_by_key(|log| Arc::as_ptr(&log.inner) as usize);
        logs.dedup_by(|a, b| Arc::ptr_eq(&a.inner, &b.inner));
        let index = |log: &Self| {
            logs.iter()
                .position(|candidate| Arc::ptr_eq(&log.inner, &candidate.inner))
                .expect("merge handle present")
        };
        let destination_index = index(self);
        let source_index = index(other);
        let baseline_index = baseline.map(index);
        let mut states: Vec<_> = logs.iter().map(|log| log.lock()).collect();
        let result = (|| -> Result<()> {
            for state in &states {
                if let Some(failure) = &state.failure {
                    return Err(eyre!("diagnostic journal unavailable: {failure}"));
                }
            }
            if states[source_index].journal.is_none() {
                return Ok(());
            }
            if states[destination_index].journal.is_none() {
                states[destination_index].journal = Some(Journal::new()?);
            }
            let destination = &states[destination_index].journal.as_ref().unwrap().conn;
            let source = &states[source_index].journal.as_ref().unwrap().conn;
            let baseline = baseline_index
                .and_then(|i| states[i].journal.as_ref())
                .map(|journal| &journal.conn);
            let mut statement = source.prepare(&format!("SELECT {EVENT_COLUMNS} FROM events"))?;
            let mut rows = statement.query([])?;
            let mut contains = baseline
                .map(|conn| conn.prepare("SELECT 1 FROM events WHERE event_key=?1"))
                .transpose()?;
            while let Some(row) = rows.next()? {
                let event = EventRow::from_row(row)?;
                let already_seen = if let Some(statement) = &mut contains {
                    statement
                        .query_row([&event.key], |_| Ok(()))
                        .optional()?
                        .is_some()
                } else {
                    false
                };
                if !already_seen {
                    event.insert(destination)?;
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            states[destination_index].failure = Some(error.to_string());
        }
    }

    /// Preserve poisoning as an infrastructure failure instead of hiding lost events.
    fn lock(&self) -> MutexGuard<'_, LogState> {
        self.inner.lock().unwrap_or_else(|poisoned| {
            let mut state = poisoned.into_inner();
            state
                .failure
                .get_or_insert_with(|| "diagnostic journal mutex poisoned".into());
            state
        })
    }
}

/// Hash nullable words without confusing missing values with an authored zero.
fn hash_word(identity: &mut Sha256, value: Option<u64>) {
    identity.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        identity.update(value.to_le_bytes());
    }
}

/// Length-prefix text and byte strings so neighboring components cannot alias.
fn hash_bytes(identity: &mut Sha256, value: Option<&[u8]>) {
    identity.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        identity.update((value.len() as u64).to_le_bytes());
        identity.update(value);
    }
}

/// Keep the longest complete UTF-8 prefix within the byte limit, flagging omissions.
fn bounded_text(value: &str, limit: usize) -> (String, bool) {
    let mut end = value.len().min(limit);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_owned(), value.len() > end)
}

/// Bound the existing console-summary examples without retaining the full event list.
pub(crate) fn bounded_summary(value: String) -> String {
    bounded_text(&value, REASON_BYTES).0
}

/// Replace the complete scalar snapshot inside the caller's provenance transaction.
/// Plugin IDs use the zero-based `record_overrides` convention, without a record FK.
pub(crate) fn write_tables<'a>(
    conn: &Connection,
    logs: impl IntoIterator<Item = (usize, &'a EventLog)>,
) -> Result<()> {
    // Check each failure while streaming, before dropping the preceding snapshot.
    // Main-database staging pages stay on disk even if the caller uses memory temp tables.
    conn.execute_batch("SAVEPOINT record_diagnostics_snapshot;")?;
    let result = write_snapshot(conn, logs);
    match result {
        Ok(()) => {
            conn.execute_batch("RELEASE record_diagnostics_snapshot;")?;
            Ok(())
        }
        Err(error) => {
            conn.execute_batch(
                "ROLLBACK TO record_diagnostics_snapshot; RELEASE record_diagnostics_snapshot;",
            )?;
            Err(error)
        }
    }
}

/// Stream into a disk staging tree before replacing the published table atomically.
fn write_snapshot<'a>(
    conn: &Connection,
    logs: impl IntoIterator<Item = (usize, &'a EventLog)>,
) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE _record_diagnostics_stage(
        plugin_id INTEGER NOT NULL, event_key BLOB NOT NULL,
        source_record_offset INTEGER, source_form_id INTEGER, form_id INTEGER,
        record_type BLOB, stage TEXT NOT NULL, field TEXT,
        occurrence INTEGER NOT NULL, link_value INTEGER,
        category TEXT NOT NULL, action TEXT NOT NULL, reason TEXT NOT NULL,
        field_truncated INTEGER NOT NULL, reason_truncated INTEGER NOT NULL,
        PRIMARY KEY(plugin_id,event_key)) WITHOUT ROWID;",
    )?;
    {
        let mut insert = conn.prepare("INSERT INTO _record_diagnostics_stage VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)
            ON CONFLICT(plugin_id,event_key) DO UPDATE SET form_id=COALESCE(_record_diagnostics_stage.form_id,excluded.form_id)")?;
        for (plugin_id, log) in logs {
            let plugin_id = i64::try_from(plugin_id)?;
            let state = log.lock();
            if let Some(failure) = &state.failure {
                return Err(eyre!("diagnostic journal unavailable: {failure}"));
            }
            let Some(journal) = &state.journal else {
                continue;
            };
            let mut statement = journal
                .conn
                .prepare(&format!("SELECT {EVENT_COLUMNS} FROM events"))?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                let event = EventRow::from_row(row)?;
                insert.execute(params![
                    plugin_id,
                    event.key,
                    event.source_record_offset,
                    event.source_form_id,
                    event.form_id,
                    event.record_type,
                    event.stage,
                    event.field,
                    event.occurrence,
                    event.link_value,
                    event.category,
                    event.action,
                    event.reason,
                    event.field_truncated,
                    event.reason_truncated,
                ])?;
            }
        }
    }
    conn.execute_batch("DROP TABLE IF EXISTS record_diagnostics;
        CREATE TABLE record_diagnostics(
            diagnostic_id INTEGER PRIMARY KEY, plugin_id INTEGER NOT NULL,
            source_record_offset INTEGER, source_form_id INTEGER, form_id INTEGER,
            record_type TEXT, stage TEXT NOT NULL, field TEXT,
            occurrence INTEGER NOT NULL, link_value INTEGER,
            category TEXT NOT NULL, action TEXT NOT NULL, reason TEXT NOT NULL,
            field_truncated INTEGER NOT NULL CHECK(field_truncated IN (0,1)),
            reason_truncated INTEGER NOT NULL CHECK(reason_truncated IN (0,1)),
            CHECK(field IS NULL OR length(CAST(field AS BLOB))<=512),
            CHECK(length(CAST(reason AS BLOB))<=2048));
        CREATE INDEX record_diagnostics_source ON record_diagnostics(plugin_id,source_record_offset);
        CREATE INDEX record_diagnostics_form ON record_diagnostics(form_id);")?;
    // A PK-ordered scan gives reproducible IDs without an event-count-sized sort buffer.
    // The physical history lookup uses its (plugin_id,source_record_offset) primary key.
    let mut source = conn.prepare(
        "SELECT s.plugin_id,s.source_record_offset,s.source_form_id,
            COALESCE(h.form_id,s.form_id),s.record_type,s.stage,s.field,s.occurrence,
            s.link_value,s.category,s.action,s.reason,s.field_truncated,s.reason_truncated
        FROM _record_diagnostics_stage s LEFT JOIN record_overrides h
        ON h.plugin_id=s.plugin_id AND h.source_record_offset=s.source_record_offset
        ORDER BY s.plugin_id,s.event_key",
    )?;
    let mut rows = source.query([])?;
    let mut earlier = conn.prepare(
        "SELECT deleted=0 FROM record_overrides
        WHERE form_id=?1 AND status='accepted'
        AND (plugin_id<?2 OR (plugin_id=?2 AND source_record_offset<?3))
        ORDER BY plugin_id DESC,source_record_offset DESC LIMIT 1",
    )?;
    let mut insert = conn.prepare("INSERT INTO record_diagnostics VALUES(NULL,?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)")?;
    while let Some(row) = rows.next()? {
        let plugin_id: i64 = row.get(0)?;
        let offset: Option<i64> = row.get(1)?;
        let form_id: Option<u32> = row.get(3)?;
        let category: String = row.get(9)?;
        let mut action: String = row.get(10)?;
        if category == "record"
            && action == "skipped"
            && form_id.is_some()
            && offset.is_some()
            && earlier
                .query_row(params![form_id, plugin_id, offset], |r| r.get::<_, bool>(0))
                .optional()?
                .unwrap_or(false)
        {
            action = "kept_earlier_version".into();
        }
        let signature: Option<Vec<u8>> = row.get(4)?;
        insert.execute(params![
            plugin_id,
            offset,
            row.get::<_, Option<u32>>(2)?,
            form_id,
            signature.as_deref().map(String::from_utf8_lossy),
            row.get::<_, String>(5)?,
            row.get::<_, Option<String>>(6)?,
            row.get::<_, i64>(7)?,
            row.get::<_, Option<u32>>(8)?,
            category,
            action,
            row.get::<_, String>(11)?,
            row.get::<_, bool>(12)?,
            row.get::<_, bool>(13)?,
        ])?;
    }
    drop(insert);
    drop(earlier);
    drop(rows);
    drop(source);
    conn.execute_batch("DROP TABLE _record_diagnostics_stage;")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Provide the existing physical-history contract without decoding plugin fixtures.
    fn database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE record_overrides(
            plugin_id INTEGER,source_record_offset INTEGER,form_id INTEGER,status TEXT,
            deleted INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY(plugin_id,source_record_offset)) WITHOUT ROWID;
            CREATE INDEX record_overrides_chain ON record_overrides(form_id);",
        )
        .unwrap();
        conn
    }

    /// Use different source/runtime IDs so enrichment cannot pass through accidental equality.
    fn context(offset: u64, occurrence: u64) -> EventContext {
        EventContext {
            source_record_offset: Some(offset),
            source_form_id: Some(0x123),
            record_type: Some(*b"REFR"),
            stage: "scan_decode",
            field: Some("enable_parent.reference[2]".into()),
            link_value: Some(0xff000456),
            occurrence,
            ..Default::default()
        }
    }

    /// Text limits preserve UTF-8, expose truncation and cannot collapse unequal long events.
    #[test]
    fn bounded_text_keeps_distinct_long_reasons() {
        let log = EventLog::default();
        let mut context = context(24, 0);
        context.field = Some(format!("{}é", "x".repeat(511)));
        let prefix = "é".repeat(1024);
        log.append(&context, "link", &format!("{prefix}first"));
        log.append(&context, "link", &format!("{prefix}second"));
        let conn = database();
        write_tables(&conn, [(0, &log)]).unwrap();
        let facts: (i64, i64, i64, i64, i64) = conn.query_row(
            "SELECT count(*),max(length(CAST(field AS BLOB))),max(length(CAST(reason AS BLOB))),sum(field_truncated),sum(reason_truncated) FROM record_diagnostics",
            [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
        ).unwrap();
        assert_eq!(facts, (2, 511, 2048, 2, 2));
        assert_eq!(bounded_summary("é".repeat(1025)).len(), 2048);
    }

    /// The fixed page cache does not become a cap on the number of retained events.
    #[test]
    fn fixed_disk_cache_retains_every_occurrence() {
        let log = EventLog::default();
        for occurrence in 0..3072 {
            log.append(&context(24, occurrence), "link", "invalid master index");
        }
        {
            let state = log.lock();
            let journal = state.journal.as_ref().unwrap();
            let cache_size: i64 = journal
                .conn
                .query_row("PRAGMA cache_size", [], |r| r.get(0))
                .unwrap();
            let temp_store: i64 = journal
                .conn
                .query_row("PRAGMA temp_store", [], |r| r.get(0))
                .unwrap();
            assert_eq!((cache_size, temp_store), (-64, 1));
            assert!(journal._path.is_file());
        }
        let conn = database();
        write_tables(&conn, [(4, &log)]).unwrap();
        let count: i64 = conn
            .query_row("SELECT count(*) FROM record_diagnostics", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 3072);
    }

    /// Exact replay duplicates disappear; physical and repeated-field occurrences survive.
    #[test]
    fn merge_preserves_occurrences_and_replay_identity() {
        let log = EventLog::default();
        log.append(&context(24, 0), "link", "bad master index");
        let replay = EventLog::default();
        let mut resolved = context(24, 0);
        resolved.form_id = Some(0x07000123);
        replay.append(&resolved, "link", "bad master index");
        replay.append(&context(24, 1), "link", "bad master index");
        replay.append(&context(80, 0), "link", "bad master index");
        log.merge_from(&replay);
        let conn = database();
        write_tables(&conn, [(7, &log)]).unwrap();
        let count: i64 = conn
            .query_row("SELECT count(*) FROM record_diagnostics", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 3);
        let resolved: u32 = conn.query_row("SELECT form_id FROM record_diagnostics WHERE source_record_offset=24 AND occurrence=0", [], |r| r.get(0)).unwrap();
        assert_eq!(resolved, 0x07000123);
    }

    /// Subtract a separate baseline using identity rather than category-level count estimates.
    #[test]
    fn merge_new_subtracts_only_exact_baseline_events() {
        let baseline = EventLog::default();
        baseline.append(&context(24, 0), "link", "invalid index");
        let resolved = EventLog::default();
        resolved.merge_from(&baseline);
        resolved.append(&context(24, 1), "link", "invalid index");
        resolved.append(&context(24, 0), "link", "wrong target kind");
        let log = EventLog::default();
        log.merge_new_from(&resolved, &baseline);
        let conn = database();
        write_tables(&conn, [(2, &log)]).unwrap();
        let count: i64 = conn
            .query_row("SELECT count(*) FROM record_diagnostics", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
        let omitted: i64 = conn.query_row("SELECT count(*) FROM record_diagnostics WHERE occurrence=0 AND reason='invalid index'", [], |r| r.get(0)).unwrap();
        assert_eq!(omitted, 0);
    }

    /// Deterministic IDs do not depend on journal insertion or plugin iteration order.
    #[test]
    fn publication_replaces_snapshot_with_stable_ids_and_history_identity() {
        let first = EventLog::default();
        first.append(&context(80, 0), "record", "truncated payload");
        first.append(&context(24, 0), "link", "invalid index");
        let other = EventLog::default();
        other.append(&context(24, 2), "field", "wrong width");
        let conn = database();
        conn.execute(
            "INSERT INTO record_overrides(plugin_id,source_record_offset,form_id,status) VALUES(0,12,?1,'accepted')",
            [0x07000123u32],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO record_overrides(plugin_id,source_record_offset,form_id,status) VALUES(7,80,?1,'skipped')",
            [0x07000123u32],
        )
        .unwrap();
        write_tables(&conn, [(7, &first), (0, &other)]).unwrap();
        let ids = |conn: &Connection| {
            conn.prepare("SELECT diagnostic_id,plugin_id,source_record_offset,reason FROM record_diagnostics ORDER BY diagnostic_id").unwrap()
                .query_map([], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?))).unwrap()
                .collect::<rusqlite::Result<Vec<_>>>().unwrap()
        };
        let before = ids(&conn);
        let reverse = EventLog::default();
        reverse.append(&context(24, 0), "link", "invalid index");
        reverse.append(&context(80, 0), "record", "truncated payload");
        write_tables(&conn, [(0, &other), (7, &reverse)]).unwrap();
        assert_eq!(before, ids(&conn));
        let recovery: (u32,String) = conn.query_row("SELECT form_id,action FROM record_diagnostics WHERE plugin_id=7 AND source_record_offset=80", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(recovery, (0x07000123, "kept_earlier_version".into()));
        write_tables(&conn, [(0, &EventLog::default())]).unwrap();
        assert!(ids(&conn).is_empty());
    }

    /// A latest accepted deletion blocks resurrection; later usable versions can restore it.
    #[test]
    fn earlier_recovery_obeys_latest_version_and_deletion_barrier() {
        let conn = database();
        conn.execute_batch(
            "INSERT INTO record_overrides VALUES
            (0,24,117440803,'accepted',0),
            (1,24,117440803,'accepted',1),
            (3,24,117440803,'accepted',0),
            (5,24,117440804,'accepted',0),
            (5,80,117440804,'accepted',1);",
        )
        .unwrap();
        let after_deleted = EventLog::default();
        let mut first = context(24, 0);
        first.form_id = Some(0x07000123);
        after_deleted.append(&first, "record", "damaged after deletion");
        let after_restored = EventLog::default();
        after_restored.append(&first, "record", "damaged after restored version");
        let same_plugin = EventLog::default();
        let mut second = context(104, 0);
        second.form_id = Some(0x07000124);
        same_plugin.append(&second, "record", "damaged after physical deletion");
        write_tables(
            &conn,
            [(2, &after_deleted), (4, &after_restored), (5, &same_plugin)],
        )
        .unwrap();
        let action = |plugin_id: u32| -> String {
            conn.query_row(
                "SELECT action FROM record_diagnostics WHERE plugin_id=?1",
                [plugin_id],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(action(2), "skipped");
        assert_eq!(action(4), "kept_earlier_version");
        assert_eq!(action(5), "skipped");
    }

    /// Physical history overrides a provisional decode ID after GMST alias reconciliation.
    #[test]
    fn physical_history_identity_is_authoritative_for_aliases() {
        let conn = database();
        conn.execute(
            "INSERT INTO record_overrides VALUES(4,80,?1,'accepted',0)",
            [0x02000123u32],
        )
        .unwrap();
        let log = EventLog::default();
        let mut provisional = context(80, 0);
        provisional.record_type = Some(*b"GMST");
        provisional.form_id = Some(0x04000123);
        log.append(&provisional, "field", "bad setting value");
        write_tables(&conn, [(4, &log)]).unwrap();
        let identity: u32 = conn
            .query_row("SELECT form_id FROM record_diagnostics", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(identity, 0x02000123);
    }

    /// An unavailable journal cannot silently publish a shorter replacement snapshot.
    #[test]
    fn infrastructure_failure_preserves_previous_snapshot() {
        let good = EventLog::default();
        good.append(&context(24, 0), "link", "invalid index");
        let conn = database();
        write_tables(&conn, [(0, &good)]).unwrap();
        let failed = EventLog::default();
        failed.append(&context(24, 0), "link", "first retained event");
        failed
            .lock()
            .journal
            .as_ref()
            .unwrap()
            .conn
            .execute_batch("DROP TABLE events")
            .unwrap();
        failed.append(&context(80, 0), "link", "cannot retain this event");
        let error = write_tables(&conn, [(1, &good), (2, &failed)]).unwrap_err();
        assert!(error.to_string().contains("diagnostic journal unavailable"));
        let reason: String = conn
            .query_row("SELECT reason FROM record_diagnostics", [], |r| r.get(0))
            .unwrap();
        assert_eq!(reason, "invalid index");
        let staging: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='_record_diagnostics_stage'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(staging, 0);
    }

    /// A publication SQL failure restores the old table even after replacement began.
    #[test]
    fn publication_failure_rolls_back_table_replacement() {
        let good = EventLog::default();
        good.append(&context(24, 0), "link", "previous event");
        let conn = database();
        write_tables(&conn, [(0, &good)]).unwrap();
        conn.execute_batch("DROP TABLE record_overrides").unwrap();
        let replacement = EventLog::default();
        replacement.append(&context(80, 0), "field", "replacement event");
        assert!(write_tables(&conn, [(1, &replacement)]).is_err());
        let reason: String = conn
            .query_row("SELECT reason FROM record_diagnostics", [], |r| r.get(0))
            .unwrap();
        assert_eq!(reason, "previous event");
    }
}
