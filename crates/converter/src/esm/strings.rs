//! Localized string tables. A plugin with the TES4 `Localized` flag stores each lstring field
//! (`FULL`, `DESC`, ...) as a 4-byte ID into `Strings/<plugin>_<language>.STRINGS` (or the
//! `.DLSTRINGS`/`.ILSTRINGS` siblings) instead of as text. The tables are loose files or entries
//! of the plugin's archives.
//!
//! Every table has the same layout: a `u32` entry count, a `u32` data size, `count` pairs of
//! `u32` ID and `u32` offset into the data, then the data. A `.STRINGS` entry is a zero-terminated
//! string at its offset; `.DLSTRINGS` and `.ILSTRINGS` entries start with a `u32` length that
//! includes the terminator.

use super::{load_order::LoadOrder, records::tes4};
use color_eyre::{Result, eyre::ensure};
use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    fs,
    ops::Range,
    path::{Path, PathBuf},
};

/// The language Skyrim uses when `sLanguage` is not set.
pub const DEFAULT_LANGUAGE: &str = "english";

/// Which of the three string table files an lstring field lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringsKind {
    /// `.STRINGS`: names and short text (`FULL`, ...).
    Strings,
    /// `.DLSTRINGS`: descriptions and book text (`DESC`, `CNAM` of BOOK, ...).
    DlStrings,
    /// `.ILSTRINGS`: dialogue (`NAM1` of INFO, ...).
    IlStrings,
}

impl StringsKind {
    fn extension(self) -> &'static str {
        match self {
            Self::Strings => "strings",
            Self::DlStrings => "dlstrings",
            Self::IlStrings => "ilstrings",
        }
    }
}

/// One parsed string table, keyed by string ID. IDs may share or overlap their bytes, so the
/// table keeps its data once and a range per ID, and decodes a string only when it is looked up:
/// memory and parsing work stay linear in the file size however the directory points into it.
#[derive(Debug, Default)]
pub struct StringTable {
    data: Vec<u8>,
    ranges: HashMap<u32, Range<usize>>,
}

impl StringTable {
    /// Parses a whole table. An entry whose offset or length points outside the data makes the
    /// table invalid, since the rest of the directory cannot be trusted either.
    pub fn parse(bytes: &[u8], kind: StringsKind) -> Result<Self> {
        let u32_at = |offset: usize| -> Option<u32> {
            let field = bytes.get(offset..offset.checked_add(4)?)?;
            Some(u32::from_le_bytes(field.try_into().ok()?))
        };
        let (Some(count), Some(data_size)) = (u32_at(0), u32_at(4)) else {
            color_eyre::eyre::bail!("string table is shorter than its header");
        };
        let directory_end = (count as usize)
            .checked_mul(8)
            .and_then(|size| size.checked_add(8))
            .filter(|&end| end <= bytes.len())
            .ok_or_else(|| {
                color_eyre::eyre::eyre!("string table directory of {count} entries is truncated")
            })?;
        let data = &bytes[directory_end..];
        ensure!(
            data.len() >= data_size as usize,
            "string table data is {} bytes, header says {data_size}",
            data.len()
        );
        let data = &data[..data_size as usize];
        // Terminator positions, found in one pass, so an entry's end is a binary search rather
        // than a rescan of its string.
        let terminators: Vec<u32> = match kind {
            StringsKind::Strings => (0..data.len() as u32)
                .filter(|&at| data[at as usize] == 0)
                .collect(),
            StringsKind::DlStrings | StringsKind::IlStrings => Vec::new(),
        };

        let mut ranges = HashMap::with_capacity(count as usize);
        for entry in 0..count as usize {
            let id = u32_at(8 + entry * 8).unwrap_or_default();
            let offset = u32_at(12 + entry * 8).unwrap_or_default() as usize;
            let range = match kind {
                StringsKind::Strings => terminators
                    .get(terminators.partition_point(|&at| (at as usize) < offset))
                    .map(|&end| offset..end as usize),
                StringsKind::DlStrings | StringsKind::IlStrings => data
                    .get(offset..offset + 4)
                    .map(|field| u32::from_le_bytes(field.try_into().unwrap()) as usize)
                    .map(|length| offset + 4..offset + 4 + length)
                    .filter(|range| range.end <= data.len())
                    .map(|range| match data[range.clone()].last() {
                        Some(0) => range.start..range.end - 1,
                        _ => range,
                    }),
            };
            let range = range.ok_or_else(|| {
                color_eyre::eyre::eyre!("string {id:08X} at offset {offset} is outside the table")
            })?;
            ranges.insert(id, range);
        }
        Ok(Self {
            data: data.to_vec(),
            ranges,
        })
    }

    pub fn get(&self, id: u32) -> Option<Cow<'_, str>> {
        self.ranges
            .get(&id)
            .map(|range| decode(&self.data[range.clone()]))
    }
}

/// Official tables are UTF-8 in some languages and Windows-1252 in others, and a table records
/// neither. A string that is not valid UTF-8 is read as Windows-1252, which every byte decodes in.
fn decode(bytes: &[u8]) -> Cow<'_, str> {
    match std::str::from_utf8(bytes) {
        Ok(text) => Cow::Borrowed(text),
        Err(_) => Cow::Owned(bytes.iter().map(|&byte| windows_1252(byte)).collect()),
    }
}

fn windows_1252(byte: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž',
        '\u{8F}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9D}',
        'ž', 'Ÿ',
    ];
    match byte {
        0x80..=0x9F => HIGH[(byte - 0x80) as usize],
        _ => char::from(byte),
    }
}

/// Where a conversion looks for string tables, and in which language.
#[derive(Debug, Clone)]
pub struct StringsSource {
    /// Folders holding a `Strings` folder, highest priority first: loose files before
    /// extracted archives, as the game resolves them.
    pub roots: Vec<PathBuf>,
    pub language: String,
}

impl Default for StringsSource {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            language: DEFAULT_LANGUAGE.to_owned(),
        }
    }
}

enum PluginTable {
    /// The plugin stores lstrings as text.
    Text,
    /// The plugin stores lstrings as IDs; `None` when its table is missing or unreadable.
    Localized(Option<StringTable>),
}

/// The `.STRINGS` tables of one load order, indexed by plugin priority, which is
/// `RawRecord::load_order`. A record's lstring IDs refer to the table of the plugin it came
/// from, so overrides resolve through the overriding plugin's table.
pub struct PluginStrings {
    names: Vec<String>,
    tables: Vec<PluginTable>,
    /// Lstrings that resolved to nothing, per plugin: count and the first record's FormID.
    unresolved: RefCell<BTreeMap<usize, (u64, u32)>>,
}

impl PluginStrings {
    /// Every plugin stores text. For exports without a load order, which cannot tell which
    /// plugin a record came from.
    pub fn text_only() -> Self {
        Self {
            names: Vec::new(),
            tables: Vec::new(),
            unresolved: RefCell::default(),
        }
    }

    /// Loads the `.STRINGS` table of every localized plugin in `order`. A missing or unreadable
    /// table is reported and leaves that plugin's names empty; it does not stop the conversion.
    pub fn load(order: &LoadOrder, source: &StringsSource) -> Self {
        let tables = order
            .names
            .iter()
            .zip(&order.metadata)
            .map(|(name, metadata)| {
                if metadata.flags & tes4::flags::LOCALIZED == 0 {
                    return PluginTable::Text;
                }
                let table = match find_table(&source.roots, name, &source.language, StringsKind::Strings) {
                    None => {
                        eprintln!(
                            "warning: {name}: localized plugin has no {} strings table; its names are left empty",
                            source.language
                        );
                        None
                    }
                    Some(path) => match fs::read(&path)
                        .map_err(Into::into)
                        .and_then(|bytes| StringTable::parse(&bytes, StringsKind::Strings))
                    {
                        Ok(table) => Some(table),
                        Err(error) => {
                            eprintln!(
                                "warning: {name}: cannot read {}: {error:#}; its names are left empty",
                                path.display()
                            );
                            None
                        }
                    },
                };
                PluginTable::Localized(table)
            })
            .collect();
        Self {
            names: order.names.clone(),
            tables,
            unresolved: RefCell::default(),
        }
    }

    /// Resolves an lstring field of a record from the plugin at `priority`. A localized plugin's
    /// field is a string ID, where 0 means no string; one that is not 4 bytes or names a missing
    /// string gives `None` and is counted for [`Self::report`].
    pub fn lstring(&self, priority: u32, form_id: u32, field: Option<&[u8]>) -> Option<String> {
        let field = field?;
        let table = match self.tables.get(priority as usize) {
            None | Some(PluginTable::Text) => {
                return Some(String::from_utf8_lossy(field).trim_matches('\0').to_owned());
            }
            Some(PluginTable::Localized(table)) => table.as_ref(),
        };
        let id = field.try_into().ok().map(u32::from_le_bytes);
        if id == Some(0) {
            return None;
        }
        let text = id.zip(table).and_then(|(id, table)| table.get(id));
        if text.is_none() && table.is_some() {
            let mut unresolved = self.unresolved.borrow_mut();
            unresolved
                .entry(priority as usize)
                .or_insert((0, form_id))
                .0 += 1;
        }
        text.map(Cow::into_owned)
    }

    /// Prints one summary per plugin whose lstrings did not resolve.
    pub fn report(&self) {
        for (&priority, &(count, example)) in self.unresolved.borrow().iter() {
            eprintln!(
                "warning: {}: {count} localized strings were not in its strings table and were left empty (first: record {example:08X})",
                self.names[priority]
            );
        }
    }
}

/// Finds `Strings/<plugin stem>_<language>.<kind>` in the first root that has it, ignoring case:
/// archives store lowercase paths, loose installs keep whatever case the files shipped with.
pub fn find_table(
    roots: &[PathBuf],
    plugin: &str,
    language: &str,
    kind: StringsKind,
) -> Option<PathBuf> {
    let stem = Path::new(plugin).file_stem()?.to_string_lossy();
    let file_name = format!("{stem}_{language}.{}", kind.extension());
    for root in roots {
        let mut candidates: Vec<_> = entries_ignoring_case(root, "strings")
            .into_iter()
            .filter(|path| path.is_dir())
            .flat_map(|folder| entries_ignoring_case(&folder, &file_name))
            .filter(|path| path.is_file())
            .collect();
        candidates.sort();
        match candidates.len() {
            0 => continue,
            1 => return candidates.pop(),
            _ => {
                eprintln!(
                    "warning: ambiguous localized table {file_name} in {}: {candidates:?}; refusing lower-priority fallback",
                    root.display()
                );
                return None;
            }
        }
    }
    None
}

fn entries_ignoring_case(folder: &Path, name: &str) -> Vec<PathBuf> {
    fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|entry_name| entry_name.eq_ignore_ascii_case(name))
        })
        .map(|entry| entry.path())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a table in the on-disk layout; offsets follow the order of `entries`.
    fn table(kind: StringsKind, entries: &[(u32, &[u8])]) -> Vec<u8> {
        let mut directory = Vec::new();
        let mut data = Vec::new();
        for (id, text) in entries {
            directory.extend(id.to_le_bytes());
            directory.extend((data.len() as u32).to_le_bytes());
            if kind != StringsKind::Strings {
                data.extend((text.len() as u32 + 1).to_le_bytes());
            }
            data.extend(*text);
            data.push(0);
        }
        [
            (entries.len() as u32).to_le_bytes().as_slice(),
            &(data.len() as u32).to_le_bytes(),
            &directory,
            &data,
        ]
        .concat()
    }

    #[test]
    fn reads_every_table_kind() {
        for kind in [
            StringsKind::Strings,
            StringsKind::DlStrings,
            StringsKind::IlStrings,
        ] {
            let bytes = table(kind, &[(0x1234, b"Lydia"), (7, b""), (9, b"Ysolda")]);
            let table = StringTable::parse(&bytes, kind).unwrap();
            assert_eq!(table.get(0x1234).as_deref(), Some("Lydia"), "{kind:?}");
            assert_eq!(table.get(7).as_deref(), Some(""), "{kind:?}");
            assert_eq!(table.get(9).as_deref(), Some("Ysolda"), "{kind:?}");
            assert_eq!(table.get(8), None, "{kind:?}");
        }
    }

    #[test]
    fn reads_windows_1252_strings_that_are_not_utf8() {
        let bytes = table(
            StringsKind::Strings,
            &[(1, b"J\xE9r\x96me"), (2, "Jérôme".as_bytes())],
        );
        let table = StringTable::parse(&bytes, StringsKind::Strings).unwrap();
        assert_eq!(table.get(1).as_deref(), Some("Jér–me"));
        assert_eq!(table.get(2).as_deref(), Some("Jérôme"));
    }

    /// Many IDs may point at one string, or into the middle of it. Storing each ID's text would
    /// let a 1 MiB table hold gigabytes; the table must stay the size of its data.
    #[test]
    fn shared_and_overlapping_offsets_do_not_multiply_the_table() {
        const LENGTH: usize = 1 << 20;
        const IDS: u32 = 8192;
        for kind in [StringsKind::Strings, StringsKind::DlStrings] {
            let mut data = Vec::new();
            if kind == StringsKind::DlStrings {
                data.extend((LENGTH as u32 + 1).to_le_bytes());
            }
            let text_start = data.len();
            data.extend(std::iter::repeat_n(b'a', LENGTH));
            data.push(0);
            let mut bytes = [IDS.to_le_bytes(), (data.len() as u32).to_le_bytes()].concat();
            for id in 0..IDS {
                // Half the IDs share the string's start; for .STRINGS the rest start inside it.
                let offset = match kind {
                    StringsKind::Strings if id % 2 == 1 => text_start + id as usize,
                    _ => 0,
                };
                bytes.extend(id.to_le_bytes());
                bytes.extend((offset as u32).to_le_bytes());
            }
            bytes.extend(&data);

            let table = StringTable::parse(&bytes, kind).unwrap();
            assert_eq!(table.data.len(), data.len(), "{kind:?}");
            assert_eq!(table.ranges.len(), IDS as usize, "{kind:?}");
            assert_eq!(table.get(0).unwrap().len(), LENGTH, "{kind:?}");
            let expected = if kind == StringsKind::Strings {
                LENGTH - 101
            } else {
                LENGTH
            };
            assert_eq!(table.get(101).unwrap().len(), expected, "{kind:?}");
        }
    }

    #[test]
    fn rejects_truncated_tables() {
        let bytes = table(StringsKind::Strings, &[(1, b"Lydia")]);
        for length in [0, 7, 15, bytes.len() - 1] {
            assert!(
                StringTable::parse(&bytes[..length], StringsKind::Strings).is_err(),
                "{length} bytes"
            );
        }
        let mut unterminated = bytes.clone();
        *unterminated.last_mut().unwrap() = b'!';
        assert!(StringTable::parse(&unterminated, StringsKind::Strings).is_err());
        let mut past_end = bytes;
        past_end[12..16].copy_from_slice(&100u32.to_le_bytes());
        assert!(StringTable::parse(&past_end, StringsKind::Strings).is_err());
    }

    /// A zero ID is "no name", not a failure: only missing strings and malformed fields are
    /// counted for the warning, and a plugin whose table did not load is not warned about twice.
    #[test]
    fn counts_only_unresolved_strings_for_the_warning() {
        let strings = PluginStrings {
            names: vec!["text.esm".into(), "loaded.esm".into(), "missing.esm".into()],
            tables: vec![
                PluginTable::Text,
                PluginTable::Localized(Some(
                    StringTable::parse(
                        &table(StringsKind::Strings, &[(1, b"Lydia")]),
                        StringsKind::Strings,
                    )
                    .unwrap(),
                )),
                PluginTable::Localized(None),
            ],
            unresolved: RefCell::default(),
        };
        let id = |id: u32| id.to_le_bytes();

        assert_eq!(
            strings.lstring(0, 0x10, Some(b"Lydia\0")).as_deref(),
            Some("Lydia")
        );
        assert_eq!(
            strings.lstring(1, 0x20, Some(&id(1))).as_deref(),
            Some("Lydia")
        );
        assert_eq!(strings.lstring(1, 0x21, Some(&id(0))), None);
        assert_eq!(strings.lstring(1, 0x22, None), None);
        assert_eq!(strings.lstring(1, 0x23, Some(&id(99))), None);
        assert_eq!(strings.lstring(1, 0x24, Some(b"Ab")), None);
        assert_eq!(strings.lstring(2, 0x30, Some(&id(0))), None);
        assert_eq!(strings.lstring(2, 0x31, Some(&id(1))), None);

        // One entry: loaded.esm, for the missing string and the malformed field, first 0x23.
        assert_eq!(
            strings.unresolved.into_inner(),
            BTreeMap::from([(1, (2, 0x23))])
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn searches_all_case_sensitive_directories_and_rejects_same_root_aliases() {
        let loose = tempfile::tempdir().unwrap();
        let archived = tempfile::tempdir().unwrap();
        for folder in ["strings", "Strings"] {
            fs::create_dir(loose.path().join(folder)).unwrap();
        }
        fs::create_dir(archived.path().join("strings")).unwrap();
        fs::write(loose.path().join("strings/other_english.strings"), b"other").unwrap();
        let winning = loose.path().join("Strings/Skyrim_English.STRINGS");
        fs::write(&winning, b"winner").unwrap();
        fs::write(
            archived.path().join("strings/skyrim_english.strings"),
            b"fallback",
        )
        .unwrap();
        let roots = [loose.path().to_owned(), archived.path().to_owned()];
        assert_eq!(
            find_table(&roots, "Skyrim.esm", "english", StringsKind::Strings),
            Some(winning)
        );
        fs::write(
            loose.path().join("strings/skyrim_english.strings"),
            b"ambiguous",
        )
        .unwrap();
        assert_eq!(
            find_table(&roots, "Skyrim.esm", "english", StringsKind::Strings),
            None
        );
    }

    #[test]
    fn finds_tables_ignoring_case_and_prefers_earlier_roots() {
        let loose = tempfile::tempdir().unwrap();
        let archives = tempfile::tempdir().unwrap();
        fs::create_dir(loose.path().join("Strings")).unwrap();
        fs::write(
            loose.path().join("Strings/Skyrim_English.STRINGS"),
            b"loose",
        )
        .unwrap();
        fs::create_dir(archives.path().join("strings")).unwrap();
        fs::write(
            archives.path().join("strings/skyrim_english.strings"),
            b"bsa",
        )
        .unwrap();
        fs::write(
            archives.path().join("strings/update_english.strings"),
            b"bsa",
        )
        .unwrap();
        let roots = [loose.path().to_owned(), archives.path().to_owned()];

        let found = |plugin, language| find_table(&roots, plugin, language, StringsKind::Strings);
        assert_eq!(
            found("skyrim.esm", "english").unwrap(),
            loose.path().join("Strings/Skyrim_English.STRINGS")
        );
        assert_eq!(
            found("Update.esm", "ENGLISH").unwrap(),
            archives.path().join("strings/update_english.strings")
        );
        assert_eq!(found("skyrim.esm", "french"), None);
        assert_eq!(found("dawnguard.esm", "english"), None);
    }
}
