//! Loading-only projection of exact source tuples and resolved shadow policy.
use super::*;
use bevy::asset::UntypedAssetId;
use std::collections::BTreeSet;

#[derive(Default)]
pub(super) struct BatchInputs {
    pub(super) facts: Vec<(bool, bool, bool)>,
    gpu: Option<u64>,
    source_scope: Option<Arc<()>>,
    bindings: Vec<Option<UntypedAssetId>>,
    by_material: HashMap<UntypedAssetId, BTreeSet<usize>>,
}

impl BatchInputs {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn refresh(
        &mut self,
        gpu: u64,
        source_scope: &Arc<()>,
        count: usize,
        bindings_changed: &BTreeSet<usize>,
        sources_changed: &BTreeSet<usize>,
        materials_changed: Option<&[UntypedAssetId]>,
        binding: impl Fn(usize) -> Option<UntypedAssetId>,
        source: impl Fn(usize) -> (bool, bool),
        shadows: impl Fn(UntypedAssetId) -> bool,
    ) -> BTreeSet<usize> {
        let reset = self.gpu != Some(gpu)
            || self.facts.len() != count
            || self
                .source_scope
                .as_ref()
                .is_none_or(|old| !Arc::ptr_eq(old, source_scope));
        let mut touched = BTreeSet::new();
        if reset {
            self.gpu = Some(gpu);
            self.source_scope = Some(source_scope.clone());
            self.by_material.clear();
            self.bindings = (0..count).map(&binding).collect();
            for (batch, asset) in self.bindings.iter().enumerate() {
                if let Some(asset) = asset {
                    self.by_material.entry(*asset).or_default().insert(batch);
                }
            }
            self.facts.resize(count, (false, false, true));
            touched.extend(0..count);
        } else {
            for &batch in bindings_changed.iter().filter(|&&batch| batch < count) {
                let current = binding(batch);
                if self.bindings[batch] != current {
                    if let Some(old) = self.bindings[batch]
                        && let std::collections::hash_map::Entry::Occupied(mut entry) =
                            self.by_material.entry(old)
                    {
                        entry.get_mut().remove(&batch);
                        if entry.get().is_empty() {
                            entry.remove();
                        }
                    }
                    if let Some(asset) = current {
                        self.by_material.entry(asset).or_default().insert(batch);
                    }
                    self.bindings[batch] = current;
                }
                touched.insert(batch);
            }
            touched.extend(
                sources_changed
                    .iter()
                    .copied()
                    .filter(|&batch| batch < count),
            );
            if let Some(materials) = materials_changed {
                for asset in materials {
                    touched.extend(self.by_material.get(asset).into_iter().flatten());
                }
            } else {
                // New/replaced material collection or an expired cursor cannot
                // inherit the old policy projection. Reestablish every row.
                touched.extend(0..count);
            }
        }
        if touched.is_empty() {
            return BTreeSet::new();
        }

        #[cfg(test)]
        crate::work_counters::record_count(
            "cluster_drawability/input_projection_batches",
            touched.len() as u64,
        );
        let mut changed = BTreeSet::new();
        for batch in touched {
            let (template, fallback) = source(batch);
            let current = (
                template,
                fallback,
                self.bindings[batch].is_none_or(&shadows),
            );
            if reset || self.facts[batch] != current {
                self.facts[batch] = current;
                changed.insert(batch);
            }
        }
        #[cfg(test)]
        crate::work_counters::record_count(
            "cluster_drawability/input_changed_batches",
            changed.len() as u64,
        );
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::shader::Shader;

    #[test]
    fn unrelated_material_and_settled_inputs_do_not_visit_any_batch() {
        let mut inputs = BatchInputs::default();
        let scope = Arc::new(());
        let used = AssetId::<Shader>::default().untyped();
        let unrelated = AssetId::<Shader>::invalid().untyped();
        inputs.refresh(
            11,
            &scope,
            100,
            &BTreeSet::new(),
            &BTreeSet::new(),
            None,
            |_| Some(used),
            |_| (true, false),
            |_| true,
        );
        assert!(
            inputs
                .refresh(
                    11,
                    &scope,
                    100,
                    &BTreeSet::new(),
                    &BTreeSet::new(),
                    Some(&[unrelated]),
                    |_| panic!("unrelated binding lookup"),
                    |_| panic!("unrelated source lookup"),
                    |_| panic!("unrelated policy lookup")
                )
                .is_empty()
        );
        assert!(
            inputs
                .refresh(
                    11,
                    &scope,
                    100,
                    &BTreeSet::new(),
                    &BTreeSet::new(),
                    Some(&[]),
                    |_| panic!("settled binding lookup"),
                    |_| panic!("settled source lookup"),
                    |_| panic!("settled policy lookup")
                )
                .is_empty()
        );
    }

    #[test]
    fn shared_material_delta_visits_only_inverse_batches_and_keeps_missing_conservative() {
        let mut inputs = BatchInputs::default();
        let scope = Arc::new(());
        let used = AssetId::<Shader>::default().untyped();
        let other = AssetId::<Shader>::invalid().untyped();
        let binding = |batch| {
            Some(if batch == 7 || batch == 91 {
                used
            } else {
                other
            })
        };
        inputs.refresh(
            11,
            &scope,
            100,
            &BTreeSet::new(),
            &BTreeSet::new(),
            None,
            binding,
            |_| (true, false),
            |_| true,
        );
        let visits = std::cell::Cell::new(0);
        let delta = inputs.refresh(
            11,
            &scope,
            100,
            &BTreeSet::new(),
            &BTreeSet::new(),
            Some(&[used]),
            binding,
            |_| {
                visits.set(visits.get() + 1);
                (true, false)
            },
            |id| id != used,
        );
        assert_eq!(delta, BTreeSet::from([7, 91]));
        assert_eq!(visits.get(), 2);
        let withdrawn = inputs.refresh(
            11,
            &scope,
            100,
            &BTreeSet::from([7]),
            &BTreeSet::new(),
            Some(&[]),
            |batch| if batch == 7 { None } else { binding(batch) },
            |_| (true, false),
            |id| id != used,
        );
        assert_eq!(withdrawn, BTreeSet::from([7]));
        assert!(inputs.facts[7].2);
        assert!(!inputs.facts[91].2);
    }
}
