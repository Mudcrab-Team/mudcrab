//! Records the plugin rows that a runtime projection leaves out (#297).
//!
//! `insert_light` and `insert_emittance_sources` skip a record when its data cannot supply a row.
//! Each skip is counted here under a stable category, with a bounded sample of FormIDs, and
//! stored in the optional tables of `docs/specs/converters/db-schema.md` §18. The stored counts
//! reach `integration-report.json` and the conversion's warnings or notices, including when a
//! metadata rebuild or a cached database republishes the snapshot.

use rusqlite::{Connection, OptionalExtension, Result, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Tables created by `create_tables` so an older reused database gains them.
pub const PROJECTION_OMISSION_TABLES_SQL: &str = r#"
         CREATE TABLE IF NOT EXISTS projection_omission_stats (
             category TEXT PRIMARY KEY, severity TEXT NOT NULL, count INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS projection_omission_samples (
             category TEXT NOT NULL, form_id INTEGER NOT NULL, reason TEXT NOT NULL,
             PRIMARY KEY (category, form_id)
         ) WITHOUT ROWID;
"#;

/// FormIDs kept per category; the count stays exact.
pub const SAMPLE_LIMIT: usize = 8;

/// Why a projection wrote no row for a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OmissionCategory {
    /// A `LIGH` without `DATA` gets no `lights` row.
    LightMissingData,
    /// A `LIGH` whose `DATA` is too short for the row's required fields gets no `lights` row.
    LightShortData,
    /// An `XEMI` target that is not in the load order, or is deleted.
    EmittanceTargetAbsent,
    /// An `XEMI` target `LIGH` without a `DATA` long enough to hold its colour.
    EmittanceLightUnusableData,
    /// An `XEMI` target `REGN` whose `RDWT` names no weather.
    EmittanceRegionNoWeather,
    /// An `XEMI` target `REGN` whose weather has no Effect Lighting colours.
    EmittanceWeatherNoLighting,
    /// An `XEMI` target that is neither a `LIGH` nor a `REGN`.
    EmittanceTargetWrongType,
}

impl OmissionCategory {
    pub fn name(self) -> &'static str {
        match self {
            Self::LightMissingData => "light_missing_data",
            Self::LightShortData => "light_short_data",
            Self::EmittanceTargetAbsent => "emittance_target_absent",
            Self::EmittanceLightUnusableData => "emittance_light_unusable_data",
            Self::EmittanceRegionNoWeather => "emittance_region_no_weather",
            Self::EmittanceWeatherNoLighting => "emittance_weather_no_lighting",
            Self::EmittanceTargetWrongType => "emittance_target_wrong_type",
        }
    }

    /// A light that loses its `lights` row loses data the runtime would draw. An unresolved
    /// emittance target keeps the model's own emission, the documented fallback.
    pub fn severity(self) -> Severity {
        match self {
            Self::LightMissingData | Self::LightShortData => Severity::DataLoss,
            Self::EmittanceTargetAbsent
            | Self::EmittanceLightUnusableData
            | Self::EmittanceRegionNoWeather
            | Self::EmittanceWeatherNoLighting
            | Self::EmittanceTargetWrongType => Severity::Fallback,
        }
    }
}

/// How an omission counts toward a complete conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// A supported projection lost a row: the conversion warns and is incomplete.
    DataLoss,
    /// The runtime falls back to authored behaviour: the conversion notes it and stays complete.
    Fallback,
}

impl Severity {
    fn name(self) -> &'static str {
        match self {
            Self::DataLoss => "data_loss",
            Self::Fallback => "fallback",
        }
    }

    /// A severity this build does not know is treated as data loss, so it cannot pass silently.
    fn parse(name: &str) -> Self {
        match name {
            "fallback" => Self::Fallback,
            _ => Self::DataLoss,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OmissionSample {
    pub form_id: u32,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryOmissions {
    pub severity: Severity,
    pub count: u64,
    /// The lowest FormIDs of the category, at most [`SAMPLE_LIMIT`], in ascending order.
    pub samples: Vec<OmissionSample>,
}

/// Omissions of the last full export, keyed by category name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProjectionOmissions {
    pub categories: BTreeMap<String, CategoryOmissions>,
}

impl ProjectionOmissions {
    pub fn record(&mut self, category: OmissionCategory, form_id: u32, reason: impl Into<String>) {
        let entry = self
            .categories
            .entry(category.name().to_owned())
            .or_insert_with(|| CategoryOmissions {
                severity: category.severity(),
                count: 0,
                samples: Vec::new(),
            });
        entry.count += 1;
        // Keep the lowest FormIDs whatever order the records arrive in.
        let at = entry
            .samples
            .partition_point(|sample| sample.form_id < form_id);
        if entry
            .samples
            .get(at)
            .is_some_and(|sample| sample.form_id == form_id)
        {
            return;
        }
        if at < SAMPLE_LIMIT {
            entry.samples.insert(
                at,
                OmissionSample {
                    form_id,
                    reason: reason.into(),
                },
            );
            entry.samples.truncate(SAMPLE_LIMIT);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.categories.is_empty()
    }

    /// Replaces the stored omissions with these.
    pub fn write(&self, conn: &Connection) -> Result<()> {
        clear(conn)?;
        for (category, omissions) in &self.categories {
            conn.execute(
                "INSERT INTO projection_omission_stats(category, severity, count) VALUES (?1, ?2, ?3)",
                params![category, omissions.severity.name(), omissions.count as i64],
            )?;
            for sample in &omissions.samples {
                conn.execute(
                    "INSERT INTO projection_omission_samples(category, form_id, reason) VALUES (?1, ?2, ?3)",
                    params![category, sample.form_id, sample.reason],
                )?;
            }
        }
        Ok(())
    }

    /// Reads the omissions of the last full export; empty when the tables are absent.
    pub fn read(conn: &Connection) -> Result<Self> {
        let present: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='projection_omission_stats'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let mut omissions = Self::default();
        if present.is_none() {
            return Ok(omissions);
        }
        let mut statement =
            conn.prepare("SELECT category, severity, count FROM projection_omission_stats")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? as u64,
            ))
        })?;
        for row in rows {
            let (category, severity, count) = row?;
            omissions.categories.insert(
                category,
                CategoryOmissions {
                    severity: Severity::parse(&severity),
                    count,
                    samples: Vec::new(),
                },
            );
        }
        let mut statement = conn.prepare(
            "SELECT category, form_id, reason FROM projection_omission_samples ORDER BY category, form_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?))
        })?;
        for row in rows {
            let (category, form_id, reason) = row?;
            if let Some(entry) = omissions.categories.get_mut(&category) {
                entry.samples.push(OmissionSample { form_id, reason });
            }
        }
        Ok(omissions)
    }

    /// One terminal line per category, for a run without a report reader.
    pub fn print_summary(&self) {
        for (category, omissions) in &self.categories {
            eprintln!("warning: {}", summary_line(category, omissions));
        }
    }
}

/// `"light_short_data: 3 records (data_loss), e.g. LIGH 00000801: DATA is 12 bytes, ..."`.
pub fn summary_line(category: &str, omissions: &CategoryOmissions) -> String {
    let mut line = format!(
        "{category}: {} record{} ({})",
        omissions.count,
        if omissions.count == 1 { "" } else { "s" },
        omissions.severity.name()
    );
    if let Some(sample) = omissions.samples.first() {
        line.push_str(&format!(", e.g. {:08X}: {}", sample.form_id, sample.reason));
    }
    line
}

/// Deletes every stored omission. Called at the start of a full export.
pub fn clear(conn: &Connection) -> Result<()> {
    for table in ["projection_omission_stats", "projection_omission_samples"] {
        conn.execute(&format!("DELETE FROM {table}"), [])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_an_exact_count_and_the_lowest_form_ids() {
        let mut omissions = ProjectionOmissions::default();
        let total = SAMPLE_LIMIT as u32 + 3;
        for form_id in (0..total).rev() {
            omissions.record(OmissionCategory::LightShortData, 0x100 + form_id, "short");
        }
        let entry = &omissions.categories["light_short_data"];
        assert_eq!(entry.count, u64::from(total));
        assert_eq!(entry.severity, Severity::DataLoss);
        let ids: Vec<u32> = entry.samples.iter().map(|sample| sample.form_id).collect();
        assert_eq!(
            ids,
            (0x100..0x100 + SAMPLE_LIMIT as u32).collect::<Vec<_>>()
        );
    }

    #[test]
    fn round_trips_through_the_tables_and_reads_empty_without_them() {
        let conn = Connection::open_in_memory().unwrap();
        assert!(ProjectionOmissions::read(&conn).unwrap().is_empty());
        conn.execute_batch(PROJECTION_OMISSION_TABLES_SQL).unwrap();
        let mut omissions = ProjectionOmissions::default();
        omissions.record(OmissionCategory::LightMissingData, 0x900, "no DATA");
        omissions.record(
            OmissionCategory::EmittanceTargetWrongType,
            0x901,
            "is a STAT",
        );
        omissions.write(&conn).unwrap();
        assert_eq!(ProjectionOmissions::read(&conn).unwrap(), omissions);
        ProjectionOmissions::default().write(&conn).unwrap();
        assert!(ProjectionOmissions::read(&conn).unwrap().is_empty());
    }

    #[test]
    fn an_unknown_stored_severity_reads_as_data_loss() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(PROJECTION_OMISSION_TABLES_SQL).unwrap();
        conn.execute(
            "INSERT INTO projection_omission_stats VALUES ('future_category', 'advisory', 2)",
            [],
        )
        .unwrap();
        let read = ProjectionOmissions::read(&conn).unwrap();
        assert_eq!(
            read.categories["future_category"].severity,
            Severity::DataLoss
        );
    }
}
