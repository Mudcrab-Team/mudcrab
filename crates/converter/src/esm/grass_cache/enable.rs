//! Lazy initial-enable decisions and complete SQLite input fingerprints for grass baking.

use color_eyre::Result;
use rusqlite::{CachedStatement, Connection, OptionalExtension, types::ValueRef};
use sha2::{Digest, Sha256};
use shared::initial_enable::{EnableDecisionInput, EnableStateResolver, InitialEnableInputs};
use std::{collections::HashMap, convert::Infallible};

/// Owns SQLite values without losing malformed text bytes or floating-point bits.
enum RawValue {
    Null,
    Integer(i64),
    Real(u64),
    Text(Vec<u8>),
    Blob(Vec<u8>),
}

impl RawValue {
    /// Copy one SQLite value while preserving its storage class and exact payload.
    fn from_sql(value: ValueRef<'_>) -> Self {
        match value {
            ValueRef::Null => Self::Null,
            ValueRef::Integer(value) => Self::Integer(value),
            ValueRef::Real(value) => Self::Real(value.to_bits()),
            ValueRef::Text(value) => Self::Text(value.to_vec()),
            ValueRef::Blob(value) => Self::Blob(value.to_vec()),
        }
    }

    /// Read only an integer in the engine's unsigned flag/FormID range.
    fn unsigned(&self) -> Option<u32> {
        match self {
            Self::Integer(value) => u32::try_from(*value).ok(),
            _ => None,
        }
    }

    /// Hash a tagged, unambiguous representation, including invalid input bytes.
    fn hash(&self, hash: &mut Sha256) {
        match self {
            Self::Null => hash.update([0]),
            Self::Integer(value) => {
                hash.update([1]);
                hash.update(value.to_le_bytes());
            }
            Self::Real(bits) => {
                hash.update([2]);
                hash.update(bits.to_le_bytes());
            }
            Self::Text(bytes) | Self::Blob(bytes) => {
                hash.update([if matches!(self, Self::Text(_)) { 3 } else { 4 }]);
                hash.update((bytes.len() as u64).to_le_bytes());
                hash.update(bytes);
            }
        }
    }
}

/// Cached row evidence; malformed records remain distinguishable from absent rows.
enum EnableRow {
    Present([RawValue; 3]),
    Missing,
    Unreadable(String),
}

impl EnableRow {
    /// Apply the engine's strict integer and nonzero-parent/XESP-flags validation.
    fn inputs(&self, columns: u8) -> Option<InitialEnableInputs> {
        if columns != 0 && columns != 7 {
            return None;
        }
        let Self::Present([header, parent, flags]) = self else {
            return None;
        };
        let header_flags = header.unsigned()?;
        let parent_id = match parent {
            RawValue::Null => None,
            value => Some(value.unsigned()?),
        };
        let parent_flags = match flags {
            RawValue::Null if !parent_id.is_some_and(|id| id != 0) => 0,
            value => value.unsigned()?,
        };
        Some(InitialEnableInputs {
            header_flags,
            parent_id,
            parent_flags,
        })
    }

    /// Hash the complete raw row or the explicit missing/read-failure terminal.
    fn hash(&self, hash: &mut Sha256) {
        match self {
            Self::Present(values) => {
                hash.update([0]);
                for value in values {
                    value.hash(hash);
                }
            }
            Self::Missing => hash.update([1]),
            Self::Unreadable(error) => {
                hash.update([2]);
                hash.update((error.len() as u64).to_le_bytes());
                hash.update(error.as_bytes());
            }
        }
    }
}

/// Resolves immutable winning references lazily for one database/bake snapshot.
/// The connection must not be mutated while this cache is in use.
pub(super) struct EnableStates<'conn> {
    statement: CachedStatement<'conn>,
    columns: u8,
    rows: HashMap<u32, EnableRow>,
    resolver: EnableStateResolver,
    decisions: HashMap<u32, (bool, [u8; 32], bool)>,
}

impl<'conn> EnableStates<'conn> {
    /// Prepare a single parent lookup; absent legacy columns use engine defaults.
    /// Partial schemas are retained as local unresolved decisions, not bake errors.
    pub(super) fn new(conn: &'conn Connection) -> Result<Self> {
        let names = ["header_flags", "enable_parent_id", "enable_parent_flags"];
        let mut columns = 0u8;
        let mut schema = conn.prepare("SELECT name FROM pragma_table_info('references')")?;
        for name in schema.query_map([], |row| row.get::<_, String>(0))? {
            let name = name?;
            if let Some(index) = names.iter().position(|candidate| *candidate == name) {
                columns |= 1 << index;
            }
        }
        let projections: Vec<_> = names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                if columns & (1 << index) != 0 {
                    *name
                } else if columns == 0 && index != 1 {
                    "0"
                } else {
                    "NULL"
                }
            })
            .collect();
        let statement = conn.prepare_cached(&format!(
            "SELECT {} FROM \"references\" WHERE id=?1",
            projections.join(",")
        ))?;
        Ok(Self {
            statement,
            columns,
            rows: HashMap::new(),
            resolver: EnableStateResolver::default(),
            decisions: HashMap::new(),
        })
    }

    /// Return `(enabled, ordered_input_sha256, unresolved)` without propagating bad rows.
    /// Hidden and unresolved candidates still need their fingerprint in cell reuse keys.
    pub(super) fn reference(&mut self, id: u32) -> (bool, [u8; 32], bool) {
        if let Some(decision) = self.decisions.get(&id) {
            return *decision;
        }
        let mut enabled = false;
        let mut unresolved = true;
        // PlayerRef is built in, including when its database row is malformed.
        let inputs = if id == 0x14 {
            Some(InitialEnableInputs {
                header_flags: 0,
                parent_id: None,
                parent_flags: 0,
            })
        } else {
            load_row(&mut self.statement, &mut self.rows, id).inputs(self.columns)
        };
        if let Some(inputs) = inputs {
            let state = self.resolver.resolve(id, inputs, |parent| {
                Ok::<_, Infallible>(
                    load_row(&mut self.statement, &mut self.rows, parent).inputs(self.columns),
                )
            });
            match state {
                Ok(Ok(value)) => {
                    enabled = value;
                    unresolved = false;
                }
                Ok(Err(_)) => {}
                Err(impossible) => match impossible {},
            }
        }
        let mut hash = Sha256::new();
        hash.update(b"grass-initial-enable-v1\0");
        hash.update([self.columns]);
        hash.update(id.to_le_bytes());
        if let Some(chain) = self.resolver.decision_inputs(id) {
            for entry in chain {
                match entry {
                    EnableDecisionInput::Reference { form_id, .. } => {
                        hash.update([0]);
                        hash.update(form_id.to_le_bytes());
                        self.rows[&form_id].hash(&mut hash);
                    }
                    EnableDecisionInput::Player => hash.update([1]),
                    EnableDecisionInput::MissingParent(parent) => {
                        hash.update([2]);
                        hash.update(parent.to_le_bytes());
                        self.rows[&parent].hash(&mut hash);
                    }
                    EnableDecisionInput::Cycle(parent) => {
                        hash.update([3]);
                        hash.update(parent.to_le_bytes());
                    }
                }
            }
        } else {
            hash.update([4]);
            self.rows[&id].hash(&mut hash);
        }
        hash.update([u8::from(enabled), u8::from(unresolved)]);
        let decision = (enabled, hash.finalize().into(), unresolved);
        self.decisions.insert(id, decision);
        decision
    }
}

/// Cache only queried rows, containing SQL failures at the affected reference/parent.
fn load_row<'rows>(
    statement: &mut CachedStatement<'_>,
    rows: &'rows mut HashMap<u32, EnableRow>,
    id: u32,
) -> &'rows EnableRow {
    rows.entry(id).or_insert_with(|| {
        match statement
            .query_row([id], |row| {
                Ok([
                    RawValue::from_sql(row.get_ref(0)?),
                    RawValue::from_sql(row.get_ref(1)?),
                    RawValue::from_sql(row.get_ref(2)?),
                ])
            })
            .optional()
        {
            Ok(Some(values)) => EnableRow::Present(values),
            Ok(None) => EnableRow::Missing,
            Err(error) => EnableRow::Unreadable(error.to_string()),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create untyped fixture columns so malformed SQLite storage classes survive.
    fn database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE \"references\"(id INTEGER PRIMARY KEY,header_flags,enable_parent_id,enable_parent_flags);",
        ).unwrap();
        conn
    }

    /// Resolve from a fresh immutable snapshot, as each bake does after a DB edit.
    fn decision(conn: &Connection, id: u32) -> (bool, [u8; 32], bool) {
        EnableStates::new(conn).unwrap().reference(id)
    }

    #[test]
    /// Literal states pin header override, link inversion, PlayerRef and unused bits.
    fn engine_rules_and_lazy_loading() {
        let conn = database();
        conn.execute_batch(
            "INSERT INTO \"references\" VALUES
             (100,2048,NULL,NULL),(101,0,100,0),(102,2048,100,1),
             (103,2048,102,0),(104,0,102,1),(105,2048,20,0),
             (106,0,20,1),(107,4096,0,8),(20,'bad',99,NULL),
             (999,'unrelated corruption',NULL,NULL);",
        )
        .unwrap();
        let mut states = EnableStates::new(&conn).unwrap();
        for (id, expected) in [
            (100, false),
            (101, false),
            (102, true),
            (103, true),
            (104, false),
            (105, true),
            (106, false),
            (107, true),
            (20, true),
        ] {
            let (enabled, _, unresolved) = states.reference(id);
            assert_eq!((enabled, unresolved), (expected, false), "{id}");
        }
        assert!(!states.rows.contains_key(&20));
        assert!(!states.rows.contains_key(&999));
        assert_eq!(states.rows.len(), 8);
    }

    #[test]
    /// Missing parents and cycles stay hidden through inversions and visit orders.
    fn unresolved_chains_have_deterministic_keys() {
        let conn = database();
        conn.execute_batch(
            "INSERT INTO \"references\" VALUES
             (30,0,99,1),(31,0,32,1),(32,2048,31,0);",
        )
        .unwrap();
        let mut first = EnableStates::new(&conn).unwrap();
        let mut second = EnableStates::new(&conn).unwrap();
        let a = first.reference(31);
        second.reference(32);
        assert_eq!(a, second.reference(31));
        for id in [30, 31, 32, 99] {
            let (enabled, _, unresolved) = first.reference(id);
            assert_eq!((enabled, unresolved), (false, true));
        }
        let missing = decision(&conn, 30).1;
        conn.execute_batch("INSERT INTO \"references\" VALUES(99,2048,NULL,0);")
            .unwrap();
        assert_ne!(missing, decision(&conn, 30).1);
        assert_eq!(
            (decision(&conn, 30).0, decision(&conn, 30).2),
            (true, false)
        );
    }

    #[test]
    /// Legacy absence enables rows; every partial schema stays hidden locally.
    fn absent_and_partial_columns() {
        let legacy = Connection::open_in_memory().unwrap();
        legacy.execute_batch("CREATE TABLE \"references\"(id INTEGER PRIMARY KEY); INSERT INTO \"references\" VALUES(1);").unwrap();
        assert_eq!(
            (decision(&legacy, 1).0, decision(&legacy, 1).2),
            (true, false)
        );
        for definition in [
            "header_flags",
            "enable_parent_id",
            "enable_parent_flags",
            "header_flags,enable_parent_id",
            "header_flags,enable_parent_flags",
            "enable_parent_id,enable_parent_flags",
        ] {
            let conn = Connection::open_in_memory().unwrap();
            conn.execute_batch(&format!("CREATE TABLE \"references\"(id INTEGER PRIMARY KEY,{definition}); INSERT INTO \"references\"(id) VALUES(1);")).unwrap();
            assert_eq!(
                (decision(&conn, 1).0, decision(&conn, 1).2),
                (false, true),
                "{definition}"
            );
        }
    }

    #[test]
    /// Corrupt child/parent fields remain local, and distinct raw repairs change keys.
    fn malformed_raw_values_invalidate_without_aborting() {
        let conn = database();
        conn.execute_batch(
            "INSERT INTO \"references\" VALUES(1,0,2,1),(2,0,NULL,NULL),(3,0,NULL,NULL);",
        )
        .unwrap();
        let healthy = decision(&conn, 3);
        let mut hashes = std::collections::HashSet::new();
        for bad in [
            "-1",
            "4294967296",
            "1.5",
            "'bad'",
            "X'626164'",
            "CAST(X'FF' AS TEXT)",
            "CAST(X'FE' AS TEXT)",
            "NULL",
        ] {
            conn.execute_batch(&format!(
                "UPDATE \"references\" SET header_flags={bad} WHERE id=2;"
            ))
            .unwrap();
            let (enabled, hash, unresolved) = decision(&conn, 1);
            assert_eq!((enabled, unresolved), (false, true), "{bad}");
            assert!(
                hashes.insert(hash),
                "raw storage classes/payloads must differ: {bad}"
            );
            assert_eq!(decision(&conn, 3), healthy);
        }
        for bad in [
            "enable_parent_id='bad'",
            "enable_parent_id=-1",
            "enable_parent_id=4294967296",
            "enable_parent_flags='bad'",
            "enable_parent_flags=-1",
            "enable_parent_flags=4294967296",
            "enable_parent_id=20,enable_parent_flags=NULL",
        ] {
            conn.execute_batch("UPDATE \"references\" SET header_flags=0,enable_parent_id=NULL,enable_parent_flags=NULL WHERE id=2;").unwrap();
            conn.execute_batch(&format!("UPDATE \"references\" SET {bad} WHERE id=2;"))
                .unwrap();
            let value = decision(&conn, 1);
            assert_eq!((value.0, value.2), (false, true), "{bad}");
            assert!(hashes.insert(value.1));
        }
        conn.execute_batch("UPDATE \"references\" SET header_flags=2048,enable_parent_id=NULL,enable_parent_flags=NULL WHERE id=2;").unwrap();
        let repaired = decision(&conn, 1);
        assert_eq!((repaired.0, repaired.2), (true, false));
        assert!(!hashes.contains(&repaired.1));
        conn.execute_batch("UPDATE \"references\" SET header_flags='bad' WHERE id=1;")
            .unwrap();
        assert_eq!((decision(&conn, 1).0, decision(&conn, 1).2), (false, true));
    }

    #[test]
    /// Every chain row and raw ignored field matters, while unrelated rows do not.
    fn precise_chain_key_invalidation() {
        let conn = database();
        conn.execute_batch("INSERT INTO \"references\" VALUES(1,0,2,0),(2,2048,3,1),(3,0,NULL,NULL),(4,0,NULL,NULL);").unwrap();
        let original = decision(&conn, 1);
        let sibling = decision(&conn, 4);
        let mut hashes = std::collections::HashSet::from([original.1]);
        for change in [
            "header_flags=4096 WHERE id=1",
            "header_flags=6144 WHERE id=2",
            "enable_parent_flags=3 WHERE id=2",
            "enable_parent_id=0 WHERE id=3",
            "enable_parent_flags=0 WHERE id=3",
            "header_flags=8192 WHERE id=3",
        ] {
            conn.execute_batch(&format!("UPDATE \"references\" SET {change};"))
                .unwrap();
            let next = decision(&conn, 1);
            assert_eq!((next.0, next.2), (false, false));
            assert!(hashes.insert(next.1), "{change}");
            assert_eq!(decision(&conn, 4), sibling);
        }
        conn.execute_batch("UPDATE \"references\" SET header_flags=2048 WHERE id=4;")
            .unwrap();
        let before = decision(&conn, 1);
        conn.execute_batch("UPDATE \"references\" SET header_flags=0 WHERE id=4;")
            .unwrap();
        assert_eq!(decision(&conn, 1), before);
    }
}
