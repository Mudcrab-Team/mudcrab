//! Plugin slots and stable identities. The owner is distinct from the winning override.
use super::binary::{PluginMetadata, parse_plugin_metadata};
use color_eyre::{Result, eyre::ensure};
use serde::Serialize;
use std::{collections::HashMap, path::PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct StableId {
    pub plugin: String,
    pub local_id: u32,
}

pub struct LoadOrder {
    pub names: Vec<String>,
    pub metadata: Vec<PluginMetadata>,
    pub normal: HashMap<String, u32>,
    pub light: HashMap<String, u32>,
}

impl LoadOrder {
    pub fn read(paths: &[PathBuf]) -> Result<Self> {
        let mut result = Self {
            names: Vec::new(),
            metadata: Vec::new(),
            normal: HashMap::new(),
            light: HashMap::new(),
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
            } else {
                let slot = result.normal.len() as u32;
                ensure!(slot < 254, "too many full plugins");
                result.normal.insert(name.clone(), slot);
            }
            result.names.push(name);
            result.metadata.push(metadata);
        }
        Ok(result)
    }

    pub fn identity(&self, form_id: u32) -> Result<StableId> {
        ensure!(form_id != 0, "null reference has no stable identity");
        let (slots, slot, local_id) = if form_id >> 24 == 0xfe {
            (&self.light, (form_id >> 12) & 0xfff, form_id & 0xfff)
        } else {
            (&self.normal, form_id >> 24, form_id & 0xffffff)
        };
        let plugin = slots
            .iter()
            .find(|(_, value)| **value == slot)
            .map(|(name, _)| name.clone())
            .ok_or_else(|| color_eyre::eyre::eyre!("unresolved slot for {form_id:08X}"))?;
        Ok(StableId { plugin, local_id })
    }
}
