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
use std::{collections::HashMap, io::Read, path::PathBuf};

/// One immutable accepted order. Allocation ids, owner priority and writer slots are separate.
#[derive(Debug)]
pub struct FormKeyOrder {
    pub table: PluginTable,
    pub order: Vec<PluginId>,
    priority: HashMap<PluginId, usize>,
    identity_order: HashMap<PluginId, (u8, usize)>,
    legacy: LegacyIdView<'static>,
    full_count: usize,
    light_count: usize,
}
impl FormKeyOrder {
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
            full_count,
            light_count,
        })
    }

    /// Resolve before packing. Invalid selectors never project to self, even for source FE.
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
        ensure!(
            selector <= entry.facts.masters.len(),
            "FormID {raw:08X} exceeds original master list"
        );
        let owner = if selector == entry.facts.masters.len() {
            id
        } else {
            self.table
                .id(&entry.facts.masters[selector])
                .ok_or_else(|| eyre!("absent original owner"))?
        };
        let local = raw & 0x00ff_ffff;
        ensure!(
            !self.table.is_light(owner) || local <= 0xfff,
            "light FormID {raw:08X} is wider than 12 bits"
        );
        Ok(Some(FormKey::new(owner, local)?))
    }

    /// Explicit conventional GMST header identity; never use for links or ownership provenance.
    pub fn gmst_header_key(&self, supplier: usize, raw: u32) -> Result<Option<FormKey>> {
        if raw == 0 {
            return Ok(None);
        }
        let id = *self
            .order
            .get(supplier)
            .ok_or_else(|| eyre!("unknown supplier"))?;
        let entry = self
            .table
            .entry(id)
            .ok_or_else(|| eyre!("unknown supplier"))?;
        if (raw >> 24) as usize <= entry.facts.masters.len() {
            return self.resolve(supplier, raw, ZeroPolicy::Null);
        }
        let local = raw & 0x00ff_ffff;
        ensure!(
            !self.table.is_light(id) || local <= 0xfff,
            "wide conventional GMST local"
        );
        Ok(Some(FormKey::new(id, local)?))
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

    /// Original MAST selectors must not be mistaken for global slots or projected onto self.
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
        assert!(order.resolve(2, 0x0200_1234, ZeroPolicy::Identity).is_err());
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
        assert!(order.resolve(0, 0x1234, ZeroPolicy::Identity).is_err());
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
