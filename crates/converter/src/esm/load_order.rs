//! Plugin slots and stable identities. The owner is distinct from the winning override.
use super::binary::{PluginMetadata, parse_plugin_metadata};
use color_eyre::{Result, eyre::ensure};
use serde::Serialize;
pub(crate) use shared::plugin_selection::normalize_plugin_name;
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
};

/// Headers and both directions of the installed dependency graph.
struct PluginHeaders {
    names: Vec<String>,
    indices: HashMap<String, usize>,
    metadata: Vec<PluginMetadata>,
    dependencies: Vec<Vec<usize>>,
    dependents: Vec<Vec<usize>>,
}

impl PluginHeaders {
    /// Read headers with the existing duplicate and unusable-header checks.
    fn read(paths: &[PathBuf]) -> Result<Self> {
        let mut names = Vec::with_capacity(paths.len());
        let mut indices = HashMap::new();
        let mut metadata = Vec::with_capacity(paths.len());
        for (index, path) in paths.iter().enumerate() {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let key = normalize_plugin_name(&name);
            ensure!(
                indices.insert(key.clone(), index).is_none(),
                "duplicate plugin {key}"
            );
            names.push(name);
            metadata.push(parse_plugin_metadata(path)?);
        }
        let mut dependencies = vec![Vec::new(); paths.len()];
        let mut dependents = vec![Vec::new(); paths.len()];
        for (index, header) in metadata.iter().enumerate() {
            let mut unique = BTreeSet::new();
            for master in &header.masters {
                if let Some(&dependency) = indices.get(&normalize_plugin_name(master))
                    && unique.insert(dependency)
                {
                    dependencies[index].push(dependency);
                    dependents[dependency].push(index);
                }
            }
        }
        Ok(Self {
            names,
            indices,
            metadata,
            dependencies,
            dependents,
        })
    }

    /// Find actual cyclic components with iterative strongly connected traversal.
    /// Nodes merely depending on a cycle are not reported as members of that cycle.
    fn cycle_members(&self) -> Vec<bool> {
        let mut visited = vec![false; self.names.len()];
        let mut finished = Vec::with_capacity(self.names.len());
        for start in 0..self.names.len() {
            if visited[start] {
                continue;
            }
            visited[start] = true;
            let mut pending = vec![(start, 0)];
            while let Some((index, next)) = pending.last_mut() {
                if let Some(&dependency) = self.dependencies[*index].get(*next) {
                    *next += 1;
                    if !visited[dependency] {
                        visited[dependency] = true;
                        pending.push((dependency, 0));
                    }
                } else {
                    finished.push(*index);
                    pending.pop();
                }
            }
        }
        visited.fill(false);
        let mut cyclic = vec![false; self.names.len()];
        for start in finished.into_iter().rev() {
            if visited[start] {
                continue;
            }
            visited[start] = true;
            let mut pending = vec![start];
            let mut component = Vec::new();
            while let Some(index) = pending.pop() {
                component.push(index);
                for &dependent in &self.dependents[index] {
                    if !visited[dependent] {
                        visited[dependent] = true;
                        pending.push(dependent);
                    }
                }
            }
            if component.len() > 1 || self.dependencies[start].contains(&start) {
                for index in component {
                    cyclic[index] = true;
                }
            }
        }
        cyclic
    }

    /// Include every transitive dependent of an excluded node without recursion.
    fn dependent_closure(&self, roots: &[bool]) -> Vec<bool> {
        let mut skipped = roots.to_vec();
        let mut pending: Vec<_> = roots
            .iter()
            .enumerate()
            .filter_map(|(index, root)| root.then_some(index))
            .collect();
        while let Some(index) = pending.pop() {
            for &dependent in &self.dependents[index] {
                if !skipped[dependent] {
                    skipped[dependent] = true;
                    pending.push(dependent);
                }
            }
        }
        skipped
    }

    /// Skip cyclic plugins and dependents, emitting one bounded warning.
    fn skip_cycles(&self, notices: &mut Vec<String>) -> Vec<bool> {
        let cyclic = self.cycle_members();
        let count = cyclic.iter().filter(|member| **member).count();
        if count == 0 {
            return cyclic;
        }
        let skipped = self.dependent_closure(&cyclic);
        let examples = self
            .names
            .iter()
            .zip(&cyclic)
            .filter_map(|(name, member)| member.then_some(name))
            .take(4)
            .map(|name| diagnostic_name(name))
            .collect::<Vec<_>>()
            .join(", ");
        let more = if count > 4 {
            format!(", and {} more", count - 4)
        } else {
            String::new()
        };
        let dependent_count = skipped
            .iter()
            .zip(&cyclic)
            .filter(|(skip, member)| **skip && !**member)
            .count();
        let first_dependent = self
            .names
            .iter()
            .zip(&skipped)
            .zip(&cyclic)
            .find_map(|((name, skip), member)| (*skip && !*member).then_some(name));
        notices.push(format!(
            "cyclic plugin dependencies: skipped {count} cycle members ({examples}{more}); {}",
            dependent_diagnostic(dependent_count, first_dependent)
        ));
        skipped
    }

    /// Skip plugins the established order places before one of their masters, plus their
    /// dependents, without moving anything; one bounded warning names the first inversion.
    fn skip_inversions(
        &self,
        ordered: &[usize],
        mut skipped: Vec<bool>,
        notices: &mut Vec<String>,
    ) -> Vec<bool> {
        let mut position = vec![usize::MAX; self.names.len()];
        for (at, &index) in ordered.iter().enumerate() {
            position[index] = at;
        }
        let mut inverted = vec![false; self.names.len()];
        let mut first = None;
        for &index in ordered {
            if let Some(&master) = self.dependencies[index]
                .iter()
                .find(|&&dependency| position[dependency] > position[index])
            {
                inverted[index] = true;
                first.get_or_insert((index, master));
            }
        }
        let Some((plugin, master)) = first else {
            return skipped;
        };
        let count = inverted.iter().filter(|member| **member).count();
        let closure = self.dependent_closure(&inverted);
        let dependents = |index: usize| closure[index] && !inverted[index] && !skipped[index];
        let dependent_count = (0..self.names.len())
            .filter(|&index| dependents(index))
            .count();
        let first_dependent = (0..self.names.len())
            .find(|&index| dependents(index))
            .map(|index| &self.names[index]);
        notices.push(format!(
            "plugins listed before their masters: skipped {count} plugins (first: {} before {}); {}",
            diagnostic_name(&self.names[plugin]),
            diagnostic_name(&self.names[master]),
            dependent_diagnostic(dependent_count, first_dependent)
        ));
        for (skip, excluded) in skipped.iter_mut().zip(closure) {
            *skip |= excluded;
        }
        skipped
    }
}

/// Limit warning examples independently of the complete structured skipped-name list.
fn diagnostic_name(name: &str) -> String {
    let mut chars = name.chars();
    let mut result: String = chars
        .by_ref()
        .take(80)
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    if chars.next().is_some() {
        result.push_str("...");
    }
    result
}

/// Summarize a dependent closure with a count and one bounded example.
fn dependent_diagnostic(count: usize, first: Option<&String>) -> String {
    match first {
        Some(name) => format!(
            "skipped {count} dependent plugins (first: {})",
            diagnostic_name(name)
        ),
        None => format!("skipped {count} dependent plugins"),
    }
}

/// Enforce slot prefixes in the established order, then remove their dependent closure.
/// Removed dependents do not cause later overflow plugins to be backfilled or reordered.
/// `order_skipped` holds earlier cycle and inversion exclusions for the complete skipped list.
fn admit_slots(
    paths: Vec<PathBuf>,
    headers: &PluginHeaders,
    ordered: Vec<usize>,
    order_skipped: &[bool],
    notices: &mut Vec<String>,
    skipped_plugins: &mut Vec<String>,
) -> Vec<PathBuf> {
    let mut full = 0;
    let mut light = 0;
    let mut full_overflow = Vec::new();
    let mut light_overflow = Vec::new();
    let mut overflow = vec![false; paths.len()];
    for &index in &ordered {
        let is_light = normalize_plugin_name(&headers.names[index]).ends_with(".esl")
            || headers.metadata[index].flags & 0x200 != 0;
        let excess = if is_light {
            light += 1;
            light > 4096
        } else {
            full += 1;
            full > 254
        };
        if excess {
            overflow[index] = true;
            if is_light {
                light_overflow.push(index);
            } else {
                full_overflow.push(index);
            }
        }
    }
    let overflow_skipped = headers.dependent_closure(&overflow);
    if !full_overflow.is_empty() || !light_overflow.is_empty() {
        let mut summaries = Vec::new();
        for (kind, limit, entries) in [
            ("full", 254, &full_overflow),
            ("light", 4096, &light_overflow),
        ] {
            if let Some(&index) = entries.first() {
                summaries.push(format!("{kind} plugin slot overflow: skipped {} {kind} plugins beyond {limit} slots (first: {})", entries.len(), diagnostic_name(&headers.names[index])));
            }
        }
        let dependent_count = (0..paths.len())
            .filter(|&index| overflow_skipped[index] && !overflow[index] && !order_skipped[index])
            .count();
        let first_dependent = (0..paths.len())
            .find(|&index| overflow_skipped[index] && !overflow[index] && !order_skipped[index])
            .map(|index| &headers.names[index]);
        summaries.push(dependent_diagnostic(dependent_count, first_dependent));
        notices.push(summaries.join("; "));
    }
    let mut existing: HashSet<_> = skipped_plugins
        .iter()
        .map(|name| normalize_plugin_name(name))
        .collect();
    for (index, name) in headers.names.iter().enumerate() {
        if (order_skipped[index] || overflow_skipped[index])
            && existing.insert(normalize_plugin_name(name))
        {
            skipped_plugins.push(name.clone());
        }
    }
    ordered
        .into_iter()
        .filter(|&index| !overflow_skipped[index])
        .map(|index| paths[index].clone())
        .collect()
}

/// Whether Skyrim loads this plugin with the masters: ESM-flagged, or an `.esm`/`.esl` file.
/// The ESL flag alone (an ESL-flagged `.esp`) narrows the slot but keeps regular priority.
fn loads_with_masters(path: &Path, metadata: &PluginMetadata) -> bool {
    metadata.flags & 0x1 != 0
        || path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("esm") || ext.eq_ignore_ascii_case("esl"))
}

/// Give master files priority without moving them ahead of their regular dependencies.
/// Keep master order and each regular dependency closure's listed order, hoisting the
/// closure immediately before its master. Remaining regular plugins keep their list order.
/// Cyclic plugins and their dependents are removed before that existing ordering. Plugins
/// still placed before a master, slot overflow, and their dependents are removed afterward,
/// with bounded notices; nothing is moved to repair an inversion.
pub(crate) fn order_explicit_plugins_with_notices(
    paths: Vec<PathBuf>,
    notices: &mut Vec<String>,
    skipped_plugins: &mut Vec<String>,
) -> Result<Vec<PathBuf>> {
    let headers = PluginHeaders::read(&paths)?;
    let skipped = headers.skip_cycles(notices);
    let master_files: Vec<_> = paths
        .iter()
        .zip(&headers.metadata)
        .map(|(path, metadata)| loads_with_masters(path, metadata))
        .collect();
    let mut emitted = vec![false; paths.len()];
    let mut ordered = Vec::with_capacity(paths.len());
    for (index, is_master) in master_files.iter().enumerate() {
        if !is_master || skipped[index] {
            continue;
        }
        // A regular ESP may itself be a master's dependency. Find that closure before
        // moving the master; retain the list's order within it. The required vector
        // bounds this iterative traversal over the surviving acyclic graph.
        let mut required = vec![false; paths.len()];
        let mut pending = vec![index];
        while let Some(dependent) = pending.pop() {
            for &dependency in &headers.dependencies[dependent] {
                if !master_files[dependency] && !emitted[dependency] && !required[dependency] {
                    required[dependency] = true;
                    pending.push(dependency);
                }
            }
        }
        for (dependency, needed) in required.into_iter().enumerate() {
            if needed {
                ordered.push(dependency);
                emitted[dependency] = true;
            }
        }
        ordered.push(index);
        emitted[index] = true;
    }
    ordered.extend((0..paths.len()).filter(|&index| !emitted[index] && !skipped[index]));
    let skipped = headers.skip_inversions(&ordered, skipped, notices);
    ordered.retain(|&index| !skipped[index]);
    Ok(admit_slots(
        paths,
        &headers,
        ordered,
        &skipped,
        notices,
        skipped_plugins,
    ))
}

/// Order automatically discovered plugins by dependency, preferring ESM-flagged
/// plugins and .esm/.esl files among ready nodes, then deterministic filename order.
/// The ESL header flag alone controls slot width, not early-load priority.
/// This fallback cannot infer user-selected override priorities from plugins.txt.
/// It shares cycle/overflow recovery with explicit selection before slot validation.
/// Callers remove plugins with missing masters first (see `plugins_txt::recover_discovered`).
pub(crate) fn order_discovered_plugins_with_notices(
    paths: Vec<PathBuf>,
    notices: &mut Vec<String>,
    skipped_plugins: &mut Vec<String>,
) -> Result<Vec<PathBuf>> {
    let headers = PluginHeaders::read(&paths)?;
    let skipped = headers.skip_cycles(notices);
    let mut remaining = vec![0usize; paths.len()];
    let mut dependents = vec![Vec::new(); paths.len()];
    for (index, header) in headers.metadata.iter().enumerate() {
        if skipped[index] {
            continue;
        }
        let mut unique = BTreeSet::new();
        for master in &header.masters {
            let dependency = *headers
                .indices
                .get(&normalize_plugin_name(master))
                .ok_or_else(|| {
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
        (
            !loads_with_masters(&paths[index], &headers.metadata[index]),
            index,
        )
    };
    let mut ready = BTreeSet::new();
    for (index, count) in remaining.iter().enumerate() {
        if *count == 0 && !skipped[index] {
            ready.insert(priority(index));
        }
    }
    let mut ordered = Vec::with_capacity(paths.len());
    while let Some((_, index)) = ready.pop_first() {
        ordered.push(index);
        for &dependent in &dependents[index] {
            remaining[dependent] -= 1;
            if remaining[dependent] == 0 {
                ready.insert(priority(dependent));
            }
        }
    }
    Ok(admit_slots(
        paths,
        &headers,
        ordered,
        &skipped,
        notices,
        skipped_plugins,
    ))
}

/// Recover a profile's existing order without hoisting or inventing a new order.
/// Plugins placed before one of their masters are skipped with their dependents.
pub(crate) fn recover_ordered_plugins_with_notices(
    paths: Vec<PathBuf>,
    notices: &mut Vec<String>,
    skipped_plugins: &mut Vec<String>,
) -> Result<Vec<PathBuf>> {
    let headers = PluginHeaders::read(&paths)?;
    let skipped = headers.skip_cycles(notices);
    let mut ordered: Vec<_> = (0..paths.len()).filter(|&index| !skipped[index]).collect();
    let skipped = headers.skip_inversions(&ordered, skipped, notices);
    ordered.retain(|&index| !skipped[index]);
    Ok(admit_slots(
        paths,
        &headers,
        ordered,
        &skipped,
        notices,
        skipped_plugins,
    ))
}

/// Original owning plugin filename and plugin-local ID, independent of its load-order slot.
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
    /// Validate an already recovered plugin order and assign full and light slots.
    /// This strict API keeps metadata aligned with every supplied path; callers must
    /// remove cyclic, inverted and overflow plugins and their dependents before reading it.
    pub fn read(paths: &[PathBuf]) -> Result<Self> {
        Self::from_metadata_readers(paths.iter().map(|path| {
            let name = normalize_plugin_name(
                &path
                    .file_name()
                    .ok_or_else(|| color_eyre::eyre::eyre!("plugin has no filename"))?
                    .to_string_lossy(),
            );
            Ok((name, move || parse_plugin_metadata(path)))
        }))
    }

    /// Rebuild the same validated slot/master authority from immutable stored header facts.
    /// This does not read source files, select plugins or reorder the supplied sequence.
    /// The caller supplies source-authenticated metadata; namespace validation does not prove
    /// that supplied fields match original bytes or authenticate an edited producing database.
    pub fn from_metadata(names: Vec<String>, metadata: Vec<PluginMetadata>) -> Result<Self> {
        ensure!(
            names.len() == metadata.len(),
            "plugin/metadata length mismatch"
        );
        Self::from_metadata_readers(
            names
                .into_iter()
                .zip(metadata)
                .map(|(name, metadata)| Ok((name, move || Ok(metadata)))),
        )
    }

    /// Validate each name before reading its header, preserving the original error order.
    fn from_metadata_readers<I, F>(rows: I) -> Result<Self>
    where
        I: IntoIterator<Item = Result<(String, F)>>,
        F: FnOnce() -> Result<PluginMetadata>,
    {
        let mut result = Self {
            names: Vec::new(),
            metadata: Vec::new(),
            normal: HashMap::new(),
            light: HashMap::new(),
            normal_by_slot: Vec::new(),
            light_by_slot: Vec::new(),
        };
        for row in rows {
            let (name, read_metadata) = row?;
            let name = normalize_plugin_name(&name);
            ensure!(!result.names.contains(&name), "duplicate plugin {name}");
            let metadata = read_metadata()?;
            for master in &metadata.masters {
                ensure!(
                    result.names.contains(&normalize_plugin_name(master)),
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

#[cfg(test)]
#[path = "load_order_recovery_tests.rs"]
mod recovery_tests;
