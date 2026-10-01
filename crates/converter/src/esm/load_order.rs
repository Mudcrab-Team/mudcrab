//! Plugin slots and stable identities. The owner is distinct from the winning override.
use super::binary::{PluginMetadata, parse_plugin_metadata};
use color_eyre::{Result, eyre::ensure};
use serde::Serialize;
use std::{
    collections::{BTreeSet, HashMap},
    path::PathBuf,
};

/// Order automatically discovered plugins by dependency, preferring master/light
/// files among ready nodes, then their existing deterministic filename order.
/// This fallback cannot infer user-selected override priorities from plugins.txt.
pub(crate) fn order_discovered_plugins(paths: Vec<PathBuf>) -> Result<Vec<PathBuf>> {
    let mut indices = HashMap::new();
    let mut metadata = Vec::with_capacity(paths.len());
    for (index, path) in paths.iter().enumerate() {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        ensure!(
            indices.insert(name.clone(), index).is_none(),
            "duplicate plugin {name}"
        );
        metadata.push(parse_plugin_metadata(path)?);
    }
    let mut remaining = vec![0usize; paths.len()];
    let mut dependents = vec![Vec::new(); paths.len()];
    for (index, header) in metadata.iter().enumerate() {
        let mut unique = BTreeSet::new();
        for master in &header.masters {
            let dependency = *indices.get(&master.to_ascii_lowercase()).ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "{}: required master {master} is missing",
                    paths[index].display()
                )
            })?;
            if unique.insert(dependency) {
                remaining[index] += 1;
                dependents[dependency].push(index);
            }
        }
    }
    let priority = |index: usize| {
        let is_light_file = paths[index]
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("esl"));
        (
            !(metadata[index].flags & 0x201 != 0 || is_light_file),
            index,
        )
    };
    let mut ready = BTreeSet::new();
    for (index, count) in remaining.iter().enumerate() {
        if *count == 0 {
            ready.insert(priority(index));
        }
    }
    let mut ordered = Vec::with_capacity(paths.len());
    while let Some((_, index)) = ready.pop_first() {
        ordered.push(paths[index].clone());
        for &dependent in &dependents[index] {
            remaining[dependent] -= 1;
            if remaining[dependent] == 0 {
                ready.insert(priority(dependent));
            }
        }
    }
    ensure!(
        ordered.len() == paths.len(),
        "cyclic plugin dependencies; blocked plugins: {}",
        paths
            .iter()
            .enumerate()
            .filter(|(index, _)| remaining[*index] != 0)
            .map(|(_, path)| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(ordered)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct StableId {
    pub plugin: String,
    pub local_id: u32,
}

/// Validated plugin order with forward mappings and constant-time ownership lookup.
pub struct LoadOrder {
    pub names: Vec<String>,
    pub metadata: Vec<PluginMetadata>,
    pub normal: HashMap<String, u32>,
    pub light: HashMap<String, u32>,
    normal_by_slot: Vec<String>,
    light_by_slot: Vec<String>,
}

impl LoadOrder {
    /// Read plugin headers, validating master order and assigning full and light slots.
    pub fn read(paths: &[PathBuf]) -> Result<Self> {
        let mut result = Self {
            names: Vec::new(),
            metadata: Vec::new(),
            normal: HashMap::new(),
            light: HashMap::new(),
            normal_by_slot: Vec::new(),
            light_by_slot: Vec::new(),
        };
        for path in paths {
            let name = path
                .file_name()
                .ok_or_else(|| color_eyre::eyre::eyre!("plugin has no filename"))?
                .to_string_lossy()
                .to_ascii_lowercase();
            ensure!(!result.names.contains(&name), "duplicate plugin {name}");
            let metadata = parse_plugin_metadata(path)?;
            for master in &metadata.masters {
                ensure!(
                    result.names.contains(&master.to_ascii_lowercase()),
                    "{name}: master {master} must precede its dependent plugin"
                );
            }
            let light = name.ends_with(".esl") || metadata.flags & 0x200 != 0;
            if light {
                let slot = result.light.len() as u32;
                ensure!(slot < 4096, "too many light plugins");
                result.light.insert(name.clone(), slot);
                result.light_by_slot.push(name.clone());
            } else {
                let slot = result.normal.len() as u32;
                ensure!(slot < 254, "too many full plugins");
                result.normal.insert(name.clone(), slot);
                result.normal_by_slot.push(name.clone());
            }
            result.names.push(name);
            result.metadata.push(metadata);
        }
        Ok(result)
    }

    /// Resolve the original owning plugin, rejecting null, absent, or inconsistent slots.
    pub fn identity(&self, form_id: u32) -> Result<StableId> {
        ensure!(form_id != 0, "null reference has no stable identity");
        let (slots, forward, slot, local_id) = if form_id >> 24 == 0xfe {
            (
                &self.light_by_slot,
                &self.light,
                (form_id >> 12) & 0xfff,
                form_id & 0xfff,
            )
        } else {
            (
                &self.normal_by_slot,
                &self.normal,
                form_id >> 24,
                form_id & 0xffffff,
            )
        };
        let plugin = slots
            .get(slot as usize)
            // Forward maps are public; fail rather than return stale ownership if mutated.
            .filter(|name| forward.get(*name) == Some(&slot))
            .cloned()
            .ok_or_else(|| color_eyre::eyre::eyre!("unresolved slot for {form_id:08X}"))?;
        Ok(StableId { plugin, local_id })
    }
}
