//! Cap-free converter identities, following Mutagen's FormKey concept.
//! Source selectors always address the original MAST list; runtime FE words are not source words.
use color_eyre::{
    Result,
    eyre::{ensure, eyre},
};
use shared::{
    form_key::{FormKey, PluginId},
    legacy_id_view::LegacyIdView,
    plugin_table::{PluginFacts, PluginFlags, PluginTable},
    source_form_id::ZeroPolicy,
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io::Read,
    path::PathBuf,
    sync::Mutex,
};

/// Warning counts are isolated by calling thread and supplier, independently of slot limits.
type ProjectionCounts = HashMap<std::thread::ThreadId, BTreeMap<usize, (u64, u32)>>;

/// Physical typed source sites exclude speculative queries and deduplicate deferred replay.
type TypedObservations = HashMap<std::thread::ThreadId, BTreeSet<(usize, u64, String, u64, u32)>>;

/// Other checked typed-writer losses, grouped by supplier and projection category.
type TypedLosses = HashMap<
    std::thread::ThreadId,
    BTreeMap<(usize, &'static str), BTreeSet<(u64, String, u64, u32)>>,
>;

/// Remove invalid explicit filenames and their dependent closure before assigning supplier indices.
/// Both frontends must use the returned paths for reading, metadata and publication.
pub fn recover_plugin_paths(paths: &[PathBuf]) -> Result<(Vec<PathBuf>, Vec<String>)> {
    let names: Vec<_> = paths
        .iter()
        .map(|path| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let mut skipped: Vec<_> = names
        .iter()
        .map(|name| shared::form_key::NamedFormKey::new(name, 1).is_err())
        .collect();
    if !skipped.iter().any(|skip| *skip) {
        return Ok((paths.to_vec(), Vec::new()));
    }
    let invalid_count = skipped.iter().filter(|skip| **skip).count();
    let first = names
        .iter()
        .zip(&skipped)
        .find(|(_, skip)| **skip)
        .map(|(name, _)| {
            name.chars()
                .take(80)
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect::<String>()
        })
        .expect("at least one invalid name");
    let metadata = paths
        .iter()
        .zip(&skipped)
        .map(|(path, skip)| {
            if *skip {
                Ok(None)
            } else {
                super::binary::parse_plugin_metadata(path).map(Some)
            }
        })
        .collect::<Result<Vec<_>>>()?;
    loop {
        let rejected: std::collections::HashSet<_> = names
            .iter()
            .zip(&skipped)
            .filter(|(_, skip)| **skip)
            .map(|(name, _)| name.to_lowercase())
            .collect();
        let mut changed = false;
        for (index, header) in metadata.iter().enumerate() {
            if !skipped[index]
                && header.as_ref().is_some_and(|header| {
                    header
                        .masters
                        .iter()
                        .any(|master| rejected.contains(&master.to_lowercase()))
                })
            {
                skipped[index] = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let dependents = skipped.iter().filter(|skip| **skip).count() - invalid_count;
    let warning = format!(
        "warning: skipped {invalid_count} invalid plugin name(s) (first: {first}); skipped {dependents} dependent plugins"
    );
    eprintln!("{warning}");
    Ok((
        paths
            .iter()
            .zip(skipped)
            .filter_map(|(path, skip)| (!skip).then_some(path.clone()))
            .collect(),
        vec![warning],
    ))
}

/// One immutable accepted order. Allocation ids, owner priority and writer slots are separate.
#[derive(Debug)]
pub struct FormKeyOrder {
    pub(crate) table: PluginTable,
    pub(crate) order: Vec<PluginId>,
    priority: HashMap<PluginId, usize>,
    identity_order: HashMap<PluginId, (u8, usize)>,
    legacy: LegacyIdView<'static>,
    projections: Mutex<ProjectionCounts>,
    typed_nulls: Mutex<ProjectionCounts>,
    typed_observations: Mutex<TypedObservations>,
    typed_losses: Mutex<TypedLosses>,
    full_count: usize,
    light_count: usize,
}
impl FormKeyOrder {
    /// Inspect the immutable pack-local plugin identity table.
    pub fn table(&self) -> &PluginTable {
        &self.table
    }

    /// Inspect supplier priority independently of table allocation order.
    pub fn plugin_order(&self) -> &[PluginId] {
        &self.order
    }

    /// Read original TES4 facts without slot admission caps or invented HEDR versions.
    /// The caller supplies the established recovered official/CCC order; this does not select it.
    pub fn read(paths: &[PathBuf]) -> Result<Self> {
        let mut rows = Vec::with_capacity(paths.len());
        for path in paths {
            let metadata = super::binary::parse_plugin_metadata(path)?;
            let reader = super::mmap_reader::EsmReader::open(path)?;
            let bytes = reader.as_slice();
            let size = u32::from_le_bytes(
                bytes
                    .get(4..8)
                    .ok_or_else(|| eyre!("truncated TES4 header: {}", path.display()))?
                    .try_into()?,
            ) as usize;
            let payload = bytes
                .get(24..)
                .and_then(|rest| rest.get(..size))
                .ok_or_else(|| eyre!("truncated TES4 payload: {}", path.display()))?;
            let decoded;
            let payload = if metadata.flags & 0x40000 != 0 {
                ensure!(payload.len() >= 4, "truncated compressed TES4");
                let expected = u32::from_le_bytes(payload[..4].try_into()?) as usize;
                ensure!(expected <= 64 * 1024 * 1024, "oversized TES4");
                let mut output = Vec::new();
                flate2::read::ZlibDecoder::new(&payload[4..])
                    .take(expected as u64 + 1)
                    .read_to_end(&mut output)?;
                ensure!(output.len() == expected, "TES4 decompressed size mismatch");
                decoded = output;
                decoded.as_slice()
            } else {
                payload
            };
            let header_version = super::extractors::extract_subrecords(payload)?
                .into_iter()
                .find_map(|(tag, data)| {
                    (tag == b"HEDR")
                        .then(|| {
                            data.get(..4).map(|word| {
                                f32::from_le_bytes(word.try_into().expect("four bytes"))
                            })
                        })
                        .flatten()
                });
            let name = path
                .file_name()
                .ok_or_else(|| eyre!("plugin has no filename"))?
                .to_string_lossy()
                .into_owned();
            rows.push((
                name,
                PluginFacts {
                    flags: PluginFlags {
                        master: metadata.flags & 1 != 0,
                        light: metadata.flags & 0x200 != 0,
                        medium: metadata.flags & 0x400 != 0,
                    },
                    header_version,
                    masters: metadata.masters,
                    content_hash: None,
                },
            ));
        }
        Self::from_facts(rows)
    }

    /// Construct from authenticated facts; missing versions remain unknown, including legacy adapters.
    pub fn from_facts(rows: Vec<(String, PluginFacts)>) -> Result<Self> {
        let table = PluginTable::fresh(rows.clone())?;
        let order: Vec<_> = rows
            .iter()
            .map(|(name, _)| table.id(name).expect("allocated name"))
            .collect();
        let mut priority = HashMap::new();
        let mut identity_order = HashMap::new();
        let (mut full_count, mut light_count) = (0, 0);
        for (at, &id) in order.iter().enumerate() {
            let entry = table.entry(id).expect("allocated id");
            for master in &entry.facts.masters {
                ensure!(
                    table
                        .id(master)
                        .is_some_and(|id| priority.contains_key(&id)),
                    "{}: master {master} must precede its dependent plugin",
                    entry.display_name
                );
            }
            priority.insert(id, at);
            let (rank, ordinal) = if table.is_light(id) {
                let ordinal = light_count;
                light_count += 1;
                (1, ordinal)
            } else {
                let ordinal = full_count;
                full_count += 1;
                (0, ordinal)
            };
            identity_order.insert(id, (rank, ordinal));
        }
        let legacy = LegacyIdView::from_load_order(&table, &order)?.into_owned();
        Ok(Self {
            table,
            order,
            priority,
            identity_order,
            legacy,
            projections: Mutex::new(HashMap::new()),
            typed_nulls: Mutex::new(HashMap::new()),
            typed_observations: Mutex::new(HashMap::new()),
            typed_losses: Mutex::new(HashMap::new()),
            full_count,
            light_count,
        })
    }

    /// Resolve before packing. Every selector at or beyond MAST count selects self.
    /// Converter compatibility intentionally permits low light locals without HEDR 1.71;
    /// shared's stricter version policy remains available to callers of its resolver.
    pub fn resolve(&self, supplier: usize, raw: u32, zero: ZeroPolicy) -> Result<Option<FormKey>> {
        let id = *self
            .order
            .get(supplier)
            .ok_or_else(|| eyre!("unknown supplier {supplier}"))?;
        if raw == 0 && zero == ZeroPolicy::Null {
            return Ok(None);
        }
        let entry = self
            .table
            .entry(id)
            .ok_or_else(|| eyre!("unknown supplier"))?;
        let selector = (raw >> 24) as usize;
        let owner = if selector >= entry.facts.masters.len() {
            id
        } else {
            self.table
                .id(&entry.facts.masters[selector])
                .ok_or_else(|| eyre!("absent original owner"))?
        };
        let local = raw & 0x00ff_ffff;
        Ok(Some(FormKey::new(owner, local)?))
    }

    /// GMST still overrides by EDID; its source-word ownership uses the common self rule.
    pub fn gmst_header_key(&self, supplier: usize, raw: u32) -> Result<Option<FormKey>> {
        self.resolve(supplier, raw, ZeroPolicy::Null)
    }

    /// Count out-of-range occurrences without changing the persisted diagnostic/event formats.
    pub(crate) fn note_projection(&self, supplier: usize, raw: u32) {
        let Some(entry) = self
            .order
            .get(supplier)
            .and_then(|&id| self.table.entry(id))
        else {
            return;
        };
        if (raw >> 24) as usize > entry.facts.masters.len() {
            let mut counts = self
                .projections
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let count = counts
                .entry(std::thread::current().id())
                .or_default()
                .entry(supplier)
                .or_insert((0, raw));
            count.0 += 1;
        }
    }

    /// Start a per-thread read without mixing counts from reused or concurrent snapshots.
    pub(crate) fn start_resolution_run(&self) {
        self.projections
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&std::thread::current().id());
        self.typed_nulls
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&std::thread::current().id());
        self.typed_observations
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&std::thread::current().id());
        self.typed_losses
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&std::thread::current().id());
    }

    /// Count checked writer losses only at physical source sites, not lookup probes/replays.
    pub(crate) fn note_typed_loss(
        &self,
        supplier: usize,
        category: &'static str,
        site: (Option<u64>, Option<&str>, u64, u32),
    ) {
        let (Some(offset), Some(field), occurrence, raw) = site else {
            return;
        };
        self.typed_losses
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .entry(std::thread::current().id())
            .or_default()
            .entry((supplier, category))
            .or_default()
            .insert((offset, field.to_owned(), occurrence, raw));
    }

    /// Observe an anomalous physical source site once, ignoring temporary type-query contexts.
    pub(crate) fn note_typed_source(
        &self,
        supplier: usize,
        raw: u32,
        offset: Option<u64>,
        field: Option<&str>,
        occurrence: u64,
    ) -> bool {
        let Some(entry) = self
            .order
            .get(supplier)
            .and_then(|&id| self.table.entry(id))
        else {
            return false;
        };
        if (raw >> 24) as usize <= entry.facts.masters.len() {
            return false;
        }
        let (Some(offset), Some(field)) = (offset, field) else {
            return false;
        };
        self.typed_observations
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .entry(std::thread::current().id())
            .or_default()
            .insert((supplier, offset, field.to_owned(), occurrence, raw))
    }

    /// Count a native self link projected to NULL only by the old typed writer policy.
    pub(crate) fn note_typed_null(&self, supplier: usize, raw: u32) {
        let mut counts = self
            .typed_nulls
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let count = counts
            .entry(std::thread::current().id())
            .or_default()
            .entry(supplier)
            .or_insert((0, raw));
        count.0 += 1;
    }

    /// Keep compatibility adaptations visible separately from native resolution counts.
    pub fn legacy_projection_warnings(&self) -> Vec<String> {
        let counts = self
            .typed_nulls
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let mut warnings: Vec<_> = counts.get(&std::thread::current().id()).into_iter().flat_map(|entries| entries.iter())
            .map(|(&supplier, &(count, first))| format!(
                "warning: {}: LegacyTypedReaderProjection kept NULL for {count} native self FormKey link occurrence(s) (first source: {first:08X})",
                self.table.entry(self.order[supplier]).expect("allocated supplier").display_name))
            .collect()
        ;
        let losses = self
            .typed_losses
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(entries) = losses.get(&std::thread::current().id()) {
            for (&(supplier, category), sites) in entries {
                let first = sites.first().expect("nonempty projection sites").3;
                warnings.push(format!("warning: {}: LegacyTypedReaderProjection {category} for {} native FormKey occurrence(s) (first source: {first:08X})", self.table.entry(self.order[supplier]).expect("allocated supplier").display_name, sites.len()));
            }
        }
        warnings
    }

    /// One warning per supplier reports all projected source occurrences and the first word.
    pub fn projection_warnings(&self) -> Vec<String> {
        let counts = self
            .projections
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        counts.get(&std::thread::current().id()).into_iter().flat_map(|entries| entries.iter())
            .map(|(&supplier, &(count, first))| format!(
                "warning: {}: {count} FormID occurrence(s) past the original master list resolved to self (first: {first:08X})",
                self.table.entry(self.order[supplier]).expect("allocated supplier").display_name))
            .collect()
    }

    /// Obtain the checked writer adapter; overcapacity identities remain in this context.
    pub fn legacy_view(&self) -> Result<&LegacyIdView<'_>> {
        Ok(&self.legacy)
    }
    /// Preflight all accepted owners, including plugins with no exported records.
    pub fn legacy_available(&self) -> bool {
        self.full_count <= 254 && self.light_count <= 4096
    }
    /// One per-run writer notice; callers decline publication rather than publish a prefix.
    pub fn notice(&self) -> Option<String> {
        (!self.legacy_available()).then(|| format!("legacy FormID publication unavailable: {} full / {} light plugins exceed 254 / 4096 slots; FormKey identities remain available", self.full_count, self.light_count))
    }
    /// Winning supplier priority is separate from the identity's original owner.
    pub fn owner_priority(&self, owner: PluginId) -> Option<usize> {
        self.priority.get(&owner).copied()
    }
    /// Preserve unsigned legacy u32 ordering: full first, then light, then owner ordinal/local.
    /// The tuple depends only on the original owner, never the winning supplier.
    pub fn identity_sort_tuple(&self, key: FormKey) -> Result<(u8, usize, u32)> {
        let &(rank, ordinal) = self
            .identity_order
            .get(&key.plugin())
            .ok_or_else(|| eyre!("unknown owner"))?;
        Ok((rank, ordinal, key.local()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Replayed fields count once; speculative catalogue queries have no physical site.
    #[test]
    fn typed_counts_ignore_queries_and_replay_but_retain_distinct_occurrences() {
        let order =
            FormKeyOrder::from_facts(vec![("Example.esm".into(), PluginFacts::default())]).unwrap();
        assert!(!order.note_typed_source(0, 0x3f00_003f, None, None, 1));
        for (offset, occurrence) in [(24, 1), (24, 1), (24, 2), (48, 1)] {
            if order.note_typed_source(
                0,
                0x3f00_003f,
                Some(offset),
                Some("ONAM.object"),
                occurrence,
            ) {
                order.note_typed_null(0, 0x3f00_003f);
            }
        }
        assert!(
            order.legacy_projection_warnings()[0]
                .contains("NULL for 3 native self FormKey link occurrence(s)")
        );
        order.start_resolution_run();
        assert!(order.legacy_projection_warnings().is_empty());
        assert!(order.note_typed_source(0, 0x3f00_003f, Some(24), Some("ONAM.object"), 1));
    }

    /// Original MAST selectors differ from global slots; out-of-range selectors resolve to self.
    #[test]
    fn original_masters_and_invalid_selectors() {
        let order = FormKeyOrder::from_facts(vec![
            ("Base.esm".into(), PluginFacts::default()),
            ("Other.esp".into(), PluginFacts::default()),
            (
                "Patch.esp".into(),
                PluginFacts {
                    masters: vec!["Base.esm".into()],
                    ..Default::default()
                },
            ),
        ])
        .unwrap();
        let key = order
            .resolve(2, 0x0100_1234, ZeroPolicy::Identity)
            .unwrap()
            .unwrap();
        assert_eq!(
            order.legacy_view().unwrap().to_legacy_u32(key).unwrap(),
            0x0200_1234
        );
        assert_eq!(
            order.resolve(2, 0x0200_1234, ZeroPolicy::Identity).unwrap(),
            Some(key)
        );
        let convention = order.gmst_header_key(2, 0x0200_1234).unwrap().unwrap();
        assert_eq!(key, convention);
        assert_eq!(order.identity_sort_tuple(key).unwrap(), (0, 2, 0x1234));
    }

    /// Preserve low light locals accepted by existing converter APIs without fabricating HEDR.
    #[test]
    fn low_light_local_and_explicit_zero_policy() {
        let order =
            FormKeyOrder::from_facts(vec![("A.esl".into(), PluginFacts::default())]).unwrap();
        let key = order
            .resolve(0, 0x234, ZeroPolicy::Identity)
            .unwrap()
            .unwrap();
        assert_eq!(
            order.legacy_view().unwrap().to_legacy_u32(key).unwrap(),
            0xfe00_0234
        );
        assert_eq!(order.table.entries()[0].facts.header_version, None);
        let wide = order
            .resolve(0, 0x1234, ZeroPolicy::Identity)
            .unwrap()
            .unwrap();
        assert_eq!(wide.local(), 0x1234);
        assert!(order.legacy_view().unwrap().to_legacy_u32(wide).is_err());
        assert_eq!(order.resolve(0, 0, ZeroPolicy::Null).unwrap(), None);
        assert!(order.resolve(0, 0, ZeroPolicy::Identity).unwrap().is_some());
    }

    /// Cap-free admission retains excess owners; preflight declines the entire legacy publication.
    #[test]
    fn excess_full_owners_remain_resolvable() {
        let order = FormKeyOrder::from_facts(
            (0..255)
                .map(|n| (format!("P{n:03}.esp"), PluginFacts::default()))
                .collect(),
        )
        .unwrap();
        let key = order
            .resolve(254, 0x876, ZeroPolicy::Identity)
            .unwrap()
            .unwrap();
        assert_eq!(order.identity_sort_tuple(key).unwrap(), (0, 254, 0x876));
        assert!(!order.legacy_available());
        assert!(order.notice().unwrap().contains("255 full"));
        assert!(order.legacy_view().unwrap().to_legacy_u32(key).is_err());
    }
}
