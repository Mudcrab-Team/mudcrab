//! Read authored image-space records without assuming Skyrim's effective frame state.
//!
//! The canonical database already contains winning IMGS and WTHR payloads. This
//! reader preserves their values; loading HNAM does not map it to Bevy exposure,
//! bloom, adaptation, or a tone-curve coefficient.

use color_eyre::{
    Result,
    eyre::{WrapErr, bail, ensure, eyre},
};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use shared::records::ArchivedRecordData;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImageSpaceSlot {
    Sunrise,
    Day,
    Sunset,
    Night,
}

impl ImageSpaceSlot {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "sunrise" => Some(Self::Sunrise),
            "day" => Some(Self::Day),
            "sunset" => Some(Self::Sunset),
            "night" => Some(Self::Night),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Sunrise => "sunrise",
            Self::Day => "day",
            Self::Sunset => "sunset",
            Self::Night => "night",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Sunrise => 0,
            Self::Day => 1,
            Self::Sunset => 2,
            Self::Night => 3,
        }
    }
}

/// An explicit authored source, not a reconstruction of native weather selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImageSpaceSelection {
    Direct(u32),
    /// Legacy archives can retain file-relative IMSP links. This path accepts
    /// only the identity case verified by the stored origin metadata.
    Weather {
        form_id: u32,
        slot: ImageSpaceSlot,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageSpaceHdr {
    pub eye_adaptation_speed: f32,
    pub bloom_blur_radius: f32,
    pub bloom_threshold: f32,
    pub bloom_scale: f32,
    pub receive_bloom_threshold: f32,
    pub white: f32,
    pub sunlight_scale: f32,
    pub sky_scale: f32,
    pub eye_adaptation_strength: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageSpaceCinematic {
    pub saturation: f32,
    pub brightness: f32,
    pub contrast: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageSpaceTint {
    pub amount: f32,
    pub rgb: [f32; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageSpaceWeatherSource {
    pub form_id: u32,
    pub editor_id: Option<String>,
    pub load_order: u32,
    pub winner_plugin: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoadedImageSpace {
    pub selection: ImageSpaceSelection,
    pub source_weather: Option<ImageSpaceWeatherSource>,
    pub form_id: u32,
    pub editor_id: Option<String>,
    /// Winning override priority from `records`, not the form's origin plugin.
    pub load_order: u32,
    pub winner_plugin: Option<String>,
    /// Authored HDR values only. `None` means HNAM is absent; no defaults are invented.
    pub hdr: Option<ImageSpaceHdr>,
    pub cinematic: ImageSpaceCinematic,
    pub tint: ImageSpaceTint,
}

struct Record {
    form_id: u32,
    load_order: u32,
    winner_plugin: Option<String>,
    data: ArchivedRecordData,
}

/// Load one explicitly selected IMGS, optionally following a WTHR authored slot.
///
/// `db_path` names the SQLite file, not the converted asset directory. Unrelated
/// records are never decoded, so unsupported records elsewhere do not block use.
pub fn load_image_space(
    db_path: &Path,
    selection: ImageSpaceSelection,
) -> Result<LoadedImageSpace> {
    shared::world_assets::validate_world_database(db_path)?;
    let connection = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .wrap_err_with(|| format!("failed to open image-space database {}", db_path.display()))?;
    // Keep weather links, the selected record, and winner metadata in one read
    // snapshot if another process publishes changes while this lookup runs.
    let snapshot = connection
        .unchecked_transaction()
        .wrap_err("cannot start image-space read snapshot")?;
    let (form_id, source_weather) = match selection {
        ImageSpaceSelection::Direct(form_id) => (form_id, None),
        ImageSpaceSelection::Weather { form_id, slot } => {
            let weather = read_record(&snapshot, form_id, "WTHR")?;
            let links = required_block(&weather, b"IMSP", 16)?;
            let offset = slot.index() * 4;
            let image_space_id = u32::from_le_bytes(links[offset..offset + 4].try_into()?);
            ensure!(
                image_space_id != 0,
                "WTHR {form_id:08X} has no image space for authored {} slot",
                slot.name()
            );
            ensure_identity_weather_link(&snapshot, &weather, image_space_id)?;
            let source = ImageSpaceWeatherSource {
                form_id,
                editor_id: editor_id(&weather)?,
                load_order: weather.load_order,
                winner_plugin: weather.winner_plugin,
            };
            (image_space_id, Some(source))
        }
    };
    let record = read_record(&snapshot, form_id, "IMGS")?;
    if block(&record, b"ENAM")?.is_some() {
        bail!(
            "IMGS {form_id:08X} uses unsupported legacy ENAM image-space layout; it is not HNAM/CNAM/TNAM"
        );
    }
    let cinematic = floats::<3>(&record, b"CNAM", true)?.expect("required block checked");
    let tint = floats::<4>(&record, b"TNAM", true)?.expect("required block checked");
    let hdr = floats::<9>(&record, b"HNAM", false)?.map(|values| ImageSpaceHdr {
        eye_adaptation_speed: values[0],
        bloom_blur_radius: values[1],
        bloom_threshold: values[2],
        bloom_scale: values[3],
        receive_bloom_threshold: values[4],
        white: values[5],
        sunlight_scale: values[6],
        sky_scale: values[7],
        eye_adaptation_strength: values[8],
    });
    Ok(LoadedImageSpace {
        selection,
        source_weather,
        form_id,
        editor_id: editor_id(&record)?,
        load_order: record.load_order,
        winner_plugin: record.winner_plugin,
        hdr,
        cinematic: ImageSpaceCinematic {
            saturation: cinematic[0],
            brightness: cinematic[1],
            contrast: cinematic[2],
        },
        tint: ImageSpaceTint {
            amount: tint[0],
            rgb: [tint[1], tint[2], tint[3]],
        },
    })
}

fn ensure_identity_weather_link(
    connection: &Connection,
    weather: &Record,
    image_space_id: u32,
) -> Result<()> {
    // The older converter does not remap WTHR/IMSP. Database schema 3..9 alone
    // cannot tell global IDs from source-plugin master indices. Base-plugin
    // slot-zero forms have the same identity in either representation; a
    // priority-one override of its weather can only have that earlier plugin as
    // master. Later overrides and mod-owned links need a future normalization
    // contract, even if a same-numbered global record happens to exist.
    let candidates_are_identity =
        weather.form_id >> 24 == 0 && image_space_id >> 24 == 0 && weather.load_order <= 1;
    let identities_match = candidates_are_identity
        && origin_priority(connection, weather.form_id)? == Some(0)
        && origin_priority(connection, image_space_id)? == Some(0);
    ensure!(
        identities_match,
        "WTHR {:08X} at winning priority {} has unverified IMSP link normalization for {image_space_id:08X}; older archives may store file-relative IDs. Authored weather lookup currently requires slot-zero base-plugin forms and winner priority 0 or 1 with matching origin metadata; select the canonical IMGS directly for other cases",
        weather.form_id,
        weather.load_order
    );
    Ok(())
}

fn origin_priority(connection: &Connection, form_id: u32) -> Result<Option<u32>> {
    let mut statement = connection
        .prepare(
            "SELECT p.priority FROM formid_map f JOIN plugins p \
             ON p.name=f.plugin_name COLLATE NOCASE WHERE f.form_id=?1",
        )
        .wrap_err("cannot verify IMSP link origins; select the canonical IMGS directly")?;
    let priorities = statement
        .query_map([form_id], |row| row.get::<_, u32>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(
        priorities.len() <= 1,
        "record {form_id:08X} has ambiguous origin metadata for IMSP lookup; select the canonical IMGS directly"
    );
    Ok(priorities.into_iter().next())
}

fn read_record(connection: &Connection, form_id: u32, expected_type: &str) -> Result<Record> {
    let row: Option<(String, u32, Vec<u8>)> = connection
        .query_row(
            "SELECT record_type,load_order,data FROM records WHERE form_id=?1",
            [form_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .wrap_err_with(|| format!("cannot read canonical record {form_id:08X}"))?;
    let (record_type, load_order, bytes) = row.ok_or_else(|| {
        eyre!("selected {expected_type} {form_id:08X} is absent from canonical records")
    })?;
    ensure!(
        record_type == expected_type,
        "record {form_id:08X} is {record_type}, expected {expected_type}"
    );
    // SQLite returns a Vec<u8> without an archive-alignment guarantee. Copy into
    // aligned storage before checked rkyv validation/deserialization.
    let mut aligned = rkyv::util::AlignedVec::<16>::with_capacity(bytes.len());
    aligned.extend_from_slice(&bytes);
    let data = rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&aligned)
        .wrap_err_with(|| format!("invalid canonical archive for {expected_type} {form_id:08X}"))?;
    let mut query = connection
        .prepare("SELECT name FROM plugins WHERE priority=?1")
        .wrap_err("cannot resolve winning plugin metadata")?;
    let plugins = query
        .query_map([load_order], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(
        plugins.len() <= 1,
        "record {form_id:08X} winning priority {load_order} matches multiple plugins"
    );
    Ok(Record {
        form_id,
        load_order,
        winner_plugin: plugins.into_iter().next(),
        data,
    })
}

fn block<'a>(record: &'a Record, tag: &[u8; 4]) -> Result<Option<&'a [u8]>> {
    let mut matches = record.data.subrecords.iter().filter(|sub| &sub.tag == tag);
    let found = matches.next().map(|sub| sub.data.as_slice());
    ensure!(
        matches.next().is_none(),
        "record {:08X} has duplicate {} blocks",
        record.form_id,
        String::from_utf8_lossy(tag)
    );
    Ok(found)
}

fn required_block<'a>(record: &'a Record, tag: &[u8; 4], expected_len: usize) -> Result<&'a [u8]> {
    let bytes = block(record, tag)?.ok_or_else(|| {
        eyre!(
            "record {:08X} lacks required {} block",
            record.form_id,
            String::from_utf8_lossy(tag)
        )
    })?;
    ensure!(
        bytes.len() == expected_len,
        "record {:08X} {} length {} is unsupported; expected {expected_len}",
        record.form_id,
        String::from_utf8_lossy(tag),
        bytes.len()
    );
    Ok(bytes)
}

fn floats<const N: usize>(
    record: &Record,
    tag: &[u8; 4],
    required: bool,
) -> Result<Option<[f32; N]>> {
    if block(record, tag)?.is_none() && !required {
        return Ok(None);
    }
    let bytes = required_block(record, tag, N * 4)?;
    let mut values = [0.0; N];
    for (value, word) in values.iter_mut().zip(bytes.as_chunks::<4>().0) {
        *value = f32::from_le_bytes(*word);
    }
    ensure!(
        values.iter().all(|value| value.is_finite()),
        "record {:08X} {} contains a nonfinite float",
        record.form_id,
        String::from_utf8_lossy(tag)
    );
    Ok(Some(values))
}

fn editor_id(record: &Record) -> Result<Option<String>> {
    block(record, b"EDID")?
        .map(|bytes| {
            let bytes = bytes.strip_suffix(&[0]).unwrap_or(bytes);
            Ok(std::str::from_utf8(bytes)
                .wrap_err_with(|| format!("record {:08X} has invalid EDID text", record.form_id))?
                .to_owned())
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::records::ArchivedSubRecord;
    use tempfile::TempDir;

    fn database() -> (TempDir, Connection) {
        let directory = tempfile::tempdir().unwrap();
        let connection = Connection::open(directory.path().join("world.db")).unwrap();
        connection.execute_batch(
            "CREATE TABLE schema_info(version INTEGER); INSERT INTO schema_info VALUES(9);
             CREATE TABLE records(form_id INTEGER PRIMARY KEY,record_type TEXT NOT NULL,load_order INTEGER NOT NULL,data BLOB NOT NULL);
             CREATE TABLE plugins(id INTEGER PRIMARY KEY,name TEXT NOT NULL,priority INTEGER NOT NULL);
             INSERT INTO plugins VALUES(91,'Winning.esm',7),(12,'Weather.esm',1),(44,'Base.esm',0);
             CREATE TABLE formid_map(form_id INTEGER PRIMARY KEY,plugin_name TEXT);")
            .unwrap();
        (directory, connection)
    }

    fn float_bytes(values: &[f32]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect()
    }

    fn fields() -> Vec<ArchivedSubRecord> {
        vec![
            ArchivedSubRecord {
                tag: *b"EDID",
                data: b"AsymmetricGrade\0".to_vec(),
            },
            ArchivedSubRecord {
                tag: *b"HNAM",
                data: float_bytes(&[11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 17.0, 18.0, 19.0]),
            },
            ArchivedSubRecord {
                tag: *b"CNAM",
                data: float_bytes(&[1.33, 1.17, 1.49]),
            },
            ArchivedSubRecord {
                tag: *b"TNAM",
                data: float_bytes(&[0.23, 0.31, 0.47, 0.59]),
            },
        ]
    }

    fn insert(
        connection: &Connection,
        id: u32,
        kind: &str,
        priority: u32,
        subrecords: Vec<ArchivedSubRecord>,
    ) {
        let bytes =
            rkyv::to_bytes::<rkyv::rancor::Error>(&ArchivedRecordData { subrecords }).unwrap();
        connection
            .execute(
                "INSERT OR REPLACE INTO records VALUES(?1,?2,?3,?4)",
                rusqlite::params![id, kind, priority, bytes.as_slice()],
            )
            .unwrap();
    }

    #[test]
    fn authored_image_space_preserves_asymmetric_values_and_winner_identity() {
        let (directory, connection) = database();
        insert(&connection, 100, "IMGS", 7, fields());
        connection
            .execute("INSERT INTO formid_map VALUES(100,'Origin.esm')", [])
            .unwrap();
        // Unsupported unrelated records must not prevent a selected record loading.
        connection
            .execute("INSERT INTO records VALUES(999,'IMGS',7,x'ffff')", [])
            .unwrap();
        let loaded = load_image_space(
            &directory.path().join("world.db"),
            ImageSpaceSelection::Direct(100),
        )
        .unwrap();
        assert_eq!(loaded.form_id, 100);
        assert_eq!(loaded.editor_id.as_deref(), Some("AsymmetricGrade"));
        assert_eq!(loaded.winner_plugin.as_deref(), Some("Winning.esm"));
        assert_eq!(loaded.load_order, 7);
        assert!(loaded.source_weather.is_none());
        assert_eq!(
            loaded.cinematic,
            ImageSpaceCinematic {
                saturation: 1.33,
                brightness: 1.17,
                contrast: 1.49
            }
        );
        assert_eq!(
            loaded.tint,
            ImageSpaceTint {
                amount: 0.23,
                rgb: [0.31, 0.47, 0.59]
            }
        );
        assert_eq!(
            loaded.hdr,
            Some(ImageSpaceHdr {
                eye_adaptation_speed: 11.0,
                bloom_blur_radius: 12.0,
                bloom_threshold: 13.0,
                bloom_scale: 14.0,
                receive_bloom_threshold: 15.0,
                white: 16.0,
                sunlight_scale: 17.0,
                sky_scale: 18.0,
                eye_adaptation_strength: 19.0,
            })
        );
    }

    #[test]
    fn authored_weather_slots_resolve_distinct_records() {
        let (directory, connection) = database();
        insert(
            &connection,
            50,
            "WTHR",
            1,
            vec![
                ArchivedSubRecord {
                    tag: *b"EDID",
                    data: b"SelectedWeather\0".to_vec(),
                },
                ArchivedSubRecord {
                    tag: *b"IMSP",
                    data: [200u32, 201, 202, 203]
                        .iter()
                        .flat_map(|id| id.to_le_bytes())
                        .collect(),
                },
            ],
        );
        for id in 200..204 {
            insert(&connection, id, "IMGS", 7, fields());
            connection
                .execute("INSERT INTO formid_map VALUES(?1,'Base.esm')", [id])
                .unwrap();
        }
        connection
            .execute("INSERT INTO formid_map VALUES(50,'Base.esm')", [])
            .unwrap();
        for (index, slot) in [
            ImageSpaceSlot::Sunrise,
            ImageSpaceSlot::Day,
            ImageSpaceSlot::Sunset,
            ImageSpaceSlot::Night,
        ]
        .into_iter()
        .enumerate()
        {
            let selection = ImageSpaceSelection::Weather { form_id: 50, slot };
            let loaded = load_image_space(&directory.path().join("world.db"), selection).unwrap();
            assert_eq!(loaded.form_id, 200 + index as u32);
            assert_eq!(loaded.selection, selection);
            let weather = loaded.source_weather.unwrap();
            assert_eq!(weather.form_id, 50);
            assert_eq!(weather.load_order, 1);
            assert_eq!(weather.winner_plugin.as_deref(), Some("Weather.esm"));
            assert_eq!(ImageSpaceSlot::parse(slot.name()), Some(slot));
        }
    }

    #[test]
    fn changing_database_values_changes_loaded_grade_without_code_changes() {
        let (directory, connection) = database();
        let path = directory.path().join("world.db");
        insert(&connection, 100, "IMGS", 7, fields());
        assert_eq!(
            load_image_space(&path, ImageSpaceSelection::Direct(100))
                .unwrap()
                .cinematic
                .brightness,
            1.17
        );
        let mut changed = fields();
        changed
            .iter_mut()
            .find(|sub| sub.tag == *b"CNAM")
            .unwrap()
            .data = float_bytes(&[1.33, 2.375, 1.49]);
        insert(&connection, 100, "IMGS", 7, changed);
        assert_eq!(
            load_image_space(&path, ImageSpaceSelection::Direct(100))
                .unwrap()
                .cinematic
                .brightness,
            2.375
        );
    }

    #[test]
    fn missing_hdr_is_explicit_and_supported_older_schema_still_loads() {
        let (directory, connection) = database();
        connection
            .execute("UPDATE schema_info SET version=3", [])
            .unwrap();
        let mut without_hdr = fields();
        without_hdr.retain(|sub| sub.tag != *b"HNAM");
        insert(&connection, 100, "IMGS", 7, without_hdr);
        let loaded = load_image_space(
            &directory.path().join("world.db"),
            ImageSpaceSelection::Direct(100),
        )
        .unwrap();
        assert!(loaded.hdr.is_none());
    }

    #[test]
    fn malformed_selected_records_fail_instead_of_inventing_defaults() {
        let cases = [
            ("short HNAM", "HNAM length"),
            ("short CNAM", "CNAM length"),
            ("long TNAM", "TNAM length"),
            ("missing CNAM", "lacks required CNAM"),
            ("missing TNAM", "lacks required TNAM"),
            ("legacy", "legacy ENAM"),
            ("nonfinite HDR", "HNAM contains a nonfinite"),
            ("nonfinite grade", "CNAM contains a nonfinite"),
            ("nonfinite tint", "TNAM contains a nonfinite"),
            ("duplicate", "duplicate CNAM"),
        ];
        for (case, expected) in cases {
            let (directory, connection) = database();
            let mut values = fields();
            match case {
                "short HNAM" => {
                    values
                        .iter_mut()
                        .find(|sub| sub.tag == *b"HNAM")
                        .unwrap()
                        .data
                        .pop()
                        .unwrap();
                }
                "short CNAM" => {
                    values
                        .iter_mut()
                        .find(|sub| sub.tag == *b"CNAM")
                        .unwrap()
                        .data
                        .pop()
                        .unwrap();
                }
                "long TNAM" => values
                    .iter_mut()
                    .find(|sub| sub.tag == *b"TNAM")
                    .unwrap()
                    .data
                    .push(0),
                "missing CNAM" => values.retain(|sub| sub.tag != *b"CNAM"),
                "missing TNAM" => values.retain(|sub| sub.tag != *b"TNAM"),
                "legacy" => {
                    values.clear();
                    values.push(ArchivedSubRecord {
                        tag: *b"ENAM",
                        data: vec![0; 40],
                    });
                }
                "nonfinite HDR" => values
                    .iter_mut()
                    .find(|sub| sub.tag == *b"HNAM")
                    .unwrap()
                    .data[..4]
                    .copy_from_slice(&f32::NAN.to_le_bytes()),
                "nonfinite grade" => values
                    .iter_mut()
                    .find(|sub| sub.tag == *b"CNAM")
                    .unwrap()
                    .data[..4]
                    .copy_from_slice(&f32::INFINITY.to_le_bytes()),
                "nonfinite tint" => values
                    .iter_mut()
                    .find(|sub| sub.tag == *b"TNAM")
                    .unwrap()
                    .data[..4]
                    .copy_from_slice(&f32::NEG_INFINITY.to_le_bytes()),
                "duplicate" => values.push(ArchivedSubRecord {
                    tag: *b"CNAM",
                    data: float_bytes(&[1.0; 3]),
                }),
                _ => unreachable!(),
            }
            insert(&connection, 100, "IMGS", 7, values);
            let error = load_image_space(
                &directory.path().join("world.db"),
                ImageSpaceSelection::Direct(100),
            )
            .unwrap_err();
            assert!(error.to_string().contains(expected), "{case}: {error:?}");
        }
    }

    #[test]
    fn missing_wrong_type_and_corrupt_archive_fail_clearly() {
        let (directory, connection) = database();
        let path = directory.path().join("world.db");
        assert!(
            load_image_space(&path, ImageSpaceSelection::Direct(100))
                .unwrap_err()
                .to_string()
                .contains("is absent")
        );
        insert(&connection, 100, "WTHR", 7, fields());
        assert!(
            load_image_space(&path, ImageSpaceSelection::Direct(100))
                .unwrap_err()
                .to_string()
                .contains("expected IMGS")
        );
        connection.execute("UPDATE records SET record_type='IMGS',data=x'ffffffffffffffffffffffffffffffff' WHERE form_id=100",[]).unwrap();
        assert!(
            load_image_space(&path, ImageSpaceSelection::Direct(100))
                .unwrap_err()
                .to_string()
                .contains("invalid canonical archive")
        );
    }

    #[test]
    fn invalid_weather_links_and_ambiguous_winner_fail_clearly() {
        let (directory, connection) = database();
        let path = directory.path().join("world.db");
        let selection = ImageSpaceSelection::Weather {
            form_id: 50,
            slot: ImageSpaceSlot::Day,
        };
        insert(
            &connection,
            50,
            "WTHR",
            2,
            vec![ArchivedSubRecord {
                tag: *b"IMSP",
                data: vec![0; 12],
            }],
        );
        assert!(
            load_image_space(&path, selection)
                .unwrap_err()
                .to_string()
                .contains("IMSP length")
        );
        insert(
            &connection,
            50,
            "WTHR",
            2,
            vec![ArchivedSubRecord {
                tag: *b"IMSP",
                data: vec![0; 16],
            }],
        );
        assert!(
            load_image_space(&path, selection)
                .unwrap_err()
                .to_string()
                .contains("no image space")
        );
        insert(&connection, 100, "IMGS", 7, fields());
        connection
            .execute("INSERT INTO plugins VALUES(92,'Ambiguous.esm',7)", [])
            .unwrap();
        assert!(
            load_image_space(&path, ImageSpaceSelection::Direct(100))
                .unwrap_err()
                .to_string()
                .contains("multiple plugins")
        );
    }

    #[test]
    fn ambiguous_legacy_weather_links_never_select_a_colliding_global_record() {
        let (directory, connection) = database();
        let path = directory.path().join("world.db");
        connection
            .execute("UPDATE schema_info SET version=3", [])
            .unwrap();
        connection
            .execute("INSERT INTO plugins VALUES(92,'Later.esm',8)", [])
            .unwrap();
        // Source index 1 could name a mod's own record while global index 1
        // names an unrelated earlier plugin. Its existence proves nothing.
        let colliding_global = 0x0100_0800u32;
        insert(&connection, colliding_global, "IMGS", 7, fields());
        insert(
            &connection,
            0x0800_0900,
            "WTHR",
            8,
            vec![ArchivedSubRecord {
                tag: *b"IMSP",
                data: [colliding_global; 4]
                    .iter()
                    .flat_map(|id| id.to_le_bytes())
                    .collect(),
            }],
        );
        let selection = ImageSpaceSelection::Weather {
            form_id: 0x0800_0900,
            slot: ImageSpaceSlot::Day,
        };
        let error = load_image_space(&path, selection).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unverified IMSP link normalization")
        );
        assert_eq!(
            load_image_space(&path, ImageSpaceSelection::Direct(colliding_global))
                .unwrap()
                .form_id,
            colliding_global
        );

        // A later override can reorder masters even when the stored link's
        // high byte is zero and both same-numbered records originated in base.
        insert(
            &connection,
            50,
            "WTHR",
            8,
            vec![ArchivedSubRecord {
                tag: *b"IMSP",
                data: [200u32; 4].iter().flat_map(|id| id.to_le_bytes()).collect(),
            }],
        );
        insert(&connection, 200, "IMGS", 7, fields());
        connection
            .execute(
                "INSERT INTO formid_map VALUES(50,'Base.esm'),(200,'Base.esm')",
                [],
            )
            .unwrap();
        let error = load_image_space(
            &path,
            ImageSpaceSelection::Weather {
                form_id: 50,
                slot: ImageSpaceSlot::Day,
            },
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unverified IMSP link normalization")
        );
    }
}
