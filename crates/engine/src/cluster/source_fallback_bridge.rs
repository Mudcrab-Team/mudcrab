//! Project definitive renderer refusals into ordinary source demand, never draw permission.
use super::*;
use crate::cluster::ownership::OwnershipFacts;
use crate::cluster::source_fallback::SourceFallbackDemand;
use bevy::{ecs::change_detection::Tick, render::MainWorld};
use std::{collections::BTreeMap, sync::Weak};

#[derive(Default)]
struct Projection {
    certificate: Option<Weak<crate::cluster::reader::CertifiedPackage>>,
    scene_change: Option<Tick>,
    batches: BTreeSet<u32>,
    material_refusals: BTreeSet<u32>,
    normal_maps: Vec<bool>,
    templates: Vec<bool>,
    ownership_enabled: bool,
    instances: BTreeMap<u32, Vec<usize>>,
    projected: BTreeMap<u32, Projected>,
    reference_counts: BTreeMap<u32, usize>,
    pending_tangents: BTreeMap<u32, BTreeSet<u32>>,
    failure: Option<&'static str>,
    source_revision: Option<(u64, u64)>,
    refusal_revision: Option<u64>,
}

struct Projected {
    references: BTreeSet<u32>,
    pending_tangents: BTreeSet<u32>,
    ownership: BTreeMap<crate::cluster::reader::RecordKey, bool>,
}

#[cfg(test)]
fn references_requiring_source(
    certificate: &crate::cluster::reader::CertifiedPackage,
    scene: &ClusterScene,
    batches: &BTreeSet<u32>,
    material_refusals: &BTreeSet<u32>,
    normal_maps: &[bool],
    ownership_templates: Option<&[bool]>,
) -> Result<Projected, &'static str> {
    project_instances(
        certificate,
        scene,
        &scene.instances,
        batches,
        material_refusals,
        normal_maps,
        ownership_templates,
    )
}

fn project_instances<'a>(
    certificate: &crate::cluster::reader::CertifiedPackage,
    scene: &ClusterScene,
    instances: impl IntoIterator<Item = &'a super::super::ClusterInstance>,
    batches: &BTreeSet<u32>,
    material_refusals: &BTreeSet<u32>,
    normal_maps: &[bool],
    ownership_templates: Option<&[bool]>,
) -> Result<Projected, &'static str> {
    let mut references = BTreeSet::new();
    let mut pending_tangents = BTreeSet::new();
    let mut ownership = BTreeMap::new();
    for instance in instances {
        let ClusterInstanceIdentity::Object(key) = instance.identity else {
            continue;
        };
        let requires_source = batches.contains(&instance.batch);
        let material_refusal = material_refusals.contains(&instance.batch);
        let mut pending = false;
        if normal_maps
            .get(instance.batch as usize)
            .copied()
            .unwrap_or(false)
        {
            let mesh = scene
                .meshes
                .get(instance.mesh as usize)
                .ok_or("source fallback mesh is not present in the current scene")?;
            match mesh.prepared_tangents_usable() {
                None => {
                    // Completion is relevant even when another fact already requires stock.
                    // Only the ownership decision may proceed without waiting in that case.
                    pending_tangents.insert(instance.mesh);
                    pending = !requires_source;
                }
                Some(false) => {
                    return Err("certified normal-mapped geometry has unusable tangents");
                }
                Some(true) => {}
            }
        }
        if requires_source {
            let identity = certificate
                .record(&key)
                .ok_or("source fallback instance is outside its certificate")?;
            references.insert(identity.reference);
        }
        // Publish terminal decisions independently: a distant unresolved tangent
        // must not hold an unrelated reference. A missing template is not positive
        // qualification, even though it is absent from the definitive refusal set.
        if let Some(templates) = ownership_templates
            && !pending
            && (material_refusal || templates.get(instance.batch as usize) == Some(&true))
        {
            ownership.insert(key, requires_source);
        }
    }
    Ok(Projected {
        references,
        pending_tangents,
        ownership,
    })
}

struct ProjectionUpdate {
    reset: bool,
    ownership: BTreeMap<crate::cluster::reader::RecordKey, Option<bool>>,
    added: BTreeSet<u32>,
    removed: BTreeSet<u32>,
    // Actual instance work rather than a timing-sensitive performance assertion.
    #[cfg(test)]
    visited: usize,
}

impl Projection {
    fn update(
        &mut self,
        certificate: &Arc<crate::cluster::reader::CertifiedPackage>,
        scene: &ClusterScene,
        changed: Tick,
        sources: &Sources,
        ownership_enabled: bool,
    ) -> Result<ProjectionUpdate, &'static str> {
        let same_certificate = self.certificate.as_ref().is_some_and(|old| {
            old.upgrade()
                .is_some_and(|old| Arc::ptr_eq(&old, certificate))
        });
        let reset = !same_certificate
            || self.scene_change != Some(changed)
            || self.ownership_enabled != ownership_enabled;
        let source_revision = (sources.version, sources.template_revision);
        if !reset
            && self.source_revision == Some(source_revision)
            && self.pending_tangents.is_empty()
        {
            return Ok(ProjectionUpdate {
                reset: false,
                ownership: BTreeMap::new(),
                added: BTreeSet::new(),
                removed: BTreeSet::new(),
                #[cfg(test)]
                visited: 0,
            });
        }
        let mut affected = BTreeSet::new();
        if reset {
            *self = Self::default();
            for (index, instance) in scene.instances.iter().enumerate() {
                if matches!(instance.identity, ClusterInstanceIdentity::Object(_)) {
                    self.instances
                        .entry(instance.batch)
                        .or_default()
                        .push(index);
                }
            }
            affected.extend(self.instances.keys().copied());
        } else {
            affected.extend(
                self.batches
                    .symmetric_difference(&sources.source_fallback_batches)
                    .copied(),
            );
            affected.extend(
                self.material_refusals
                    .symmetric_difference(&sources.material_refused_batches)
                    .copied(),
            );
            for index in 0..self.normal_maps.len().max(sources.normal_maps.len()) {
                if self.normal_maps.get(index) != sources.normal_maps.get(index) {
                    affected.insert(index as u32);
                }
            }
            if ownership_enabled {
                for index in 0..self.templates.len().max(sources.templates.len()) {
                    if self.templates.get(index) != sources.templates.get(index) {
                        affected.insert(index as u32);
                    }
                }
            }
            // Worker tangent proof can complete without an ECS scene tick. Each pending mesh
            // names precisely the batches that need another qualification attempt.
            for (mesh, batches) in &self.pending_tangents {
                if scene
                    .meshes
                    .get(*mesh as usize)
                    .and_then(|mesh| mesh.prepared_tangents_usable())
                    .is_some()
                {
                    affected.extend(batches);
                }
            }
        }
        let mut update = ProjectionUpdate {
            reset,
            ownership: BTreeMap::new(),
            added: BTreeSet::new(),
            removed: BTreeSet::new(),
            #[cfg(test)]
            visited: 0,
        };
        let mut before = BTreeMap::new();
        for batch in affected {
            let Some(indices) = self.instances.get(&batch) else {
                continue;
            };
            let projected = project_instances(
                certificate,
                scene,
                indices.iter().map(|index| &scene.instances[*index]),
                &sources.source_fallback_batches,
                &sources.material_refused_batches,
                &sources.normal_maps,
                ownership_enabled.then_some(sources.templates.as_slice()),
            )?;
            #[cfg(test)]
            {
                update.visited += indices.len();
            }
            if let Some(old) = self.projected.remove(&batch) {
                for reference in old.references {
                    before.entry(reference).or_insert(true);
                    if let Some(count) = self.reference_counts.get_mut(&reference) {
                        *count -= 1;
                        if *count == 0 {
                            self.reference_counts.remove(&reference);
                        }
                    }
                }
                for mesh in old.pending_tangents {
                    if let Some(batches) = self.pending_tangents.get_mut(&mesh) {
                        batches.remove(&batch);
                        if batches.is_empty() {
                            self.pending_tangents.remove(&mesh);
                        }
                    }
                }
                update
                    .ownership
                    .extend(old.ownership.into_keys().map(|key| (key, None)));
            }
            for reference in &projected.references {
                before
                    .entry(*reference)
                    .or_insert_with(|| self.reference_counts.contains_key(reference));
                *self.reference_counts.entry(*reference).or_default() += 1;
            }
            for mesh in &projected.pending_tangents {
                self.pending_tangents
                    .entry(*mesh)
                    .or_default()
                    .insert(batch);
            }
            update.ownership.extend(
                projected
                    .ownership
                    .iter()
                    .map(|(key, value)| (*key, Some(*value))),
            );
            self.projected.insert(batch, projected);
        }
        for (reference, existed) in before {
            match (existed, self.reference_counts.contains_key(&reference)) {
                (false, true) => {
                    update.added.insert(reference);
                }
                (true, false) => {
                    update.removed.insert(reference);
                }
                _ => {}
            }
        }
        self.certificate = Some(Arc::downgrade(certificate));
        self.scene_change = Some(changed);
        self.batches.clone_from(&sources.source_fallback_batches);
        self.material_refusals
            .clone_from(&sources.material_refused_batches);
        self.normal_maps.clone_from(&sources.normal_maps);
        self.templates.clone_from(&sources.templates);
        self.ownership_enabled = ownership_enabled;
        self.failure = None;
        self.source_revision = Some(source_revision);
        Ok(update)
    }
}

fn publish(sources: Res<Sources>, mut main: ResMut<MainWorld>, mut previous: Local<Projection>) {
    let ownership_enabled = main.contains_resource::<OwnershipFacts>();
    let coherent = sources.certificate.as_ref().and_then(|certificate| {
        let scene = main.get_resource_ref::<ClusterScene>()?;
        let scene_certificate = scene.certificate.as_ref()?;
        Arc::ptr_eq(certificate, scene_certificate).then_some((certificate.clone(), scene))
    });
    let Some((certificate, scene)) = coherent else {
        if let Some(mut facts) = main.get_resource_mut::<OwnershipFacts>() {
            facts.revoke();
        }
        if let Some(mut demand) = main.get_resource_mut::<SourceFallbackDemand>() {
            demand.update(None, BTreeSet::new());
        }
        *previous = Projection::default();
        return;
    };
    let changed = scene.last_changed();
    let previous_failure = previous.failure;
    let projected = previous.update(&certificate, &scene, changed, &sources, ownership_enabled);
    // The immutable scene borrow ends before main-world publication.
    let update = match projected {
        Ok(update) => update,
        Err(reason) => {
            if previous_failure != Some(reason) {
                error!(reason, "cluster source fallback qualification refused");
            }
            if let Some(mut demand) = main.get_resource_mut::<SourceFallbackDemand>() {
                demand.update(None, BTreeSet::new());
            }
            if let Some(mut facts) = main.get_resource_mut::<OwnershipFacts>() {
                facts.revoke();
            }
            crate::cluster::package::refuse_package(&mut main, reason.to_owned());
            *previous = Projection {
                failure: Some(reason),
                ..default()
            };
            return;
        }
    };
    if let Some(mut facts) = main.get_resource_mut::<OwnershipFacts>()
        && (update.reset || !update.ownership.is_empty())
    {
        facts.publish_updates(&certificate, update.reset, update.ownership);
    }
    if ownership_enabled {
        // Runtime failures never demand stock for accepted records. Only the
        // certificate's fixed terminal material refusals can add source draws.
        let revision = main
            .get_resource::<OwnershipFacts>()
            .filter(|facts| facts.package_matches(&certificate))
            .map(|facts| facts.refusal_revision());
        if revision != previous.refusal_revision || update.reset {
            let refused = main
                .get_resource::<OwnershipFacts>()
                .filter(|facts| facts.package_matches(&certificate))
                .map(|facts| facts.permanent_refused_references());
            if let Some(refused) = refused
                && let Some(mut demand) = main.get_resource_mut::<SourceFallbackDemand>()
            {
                demand.update(Some(&certificate), refused);
            }
            previous.refusal_revision = revision;
        } else if let Some(mut demand) = main.get_resource_mut::<SourceFallbackDemand>() {
            demand.refresh_for_package(&certificate);
        }
        return;
    }
    if let Some(mut demand) = main.get_resource_mut::<SourceFallbackDemand>() {
        if previous.pending_tangents.is_empty() {
            let changed = if update.reset || demand.for_package(&certificate).is_none() {
                demand.update(
                    Some(&certificate),
                    previous.reference_counts.keys().copied().collect(),
                )
            } else if update.added.is_empty() && update.removed.is_empty() {
                demand.refresh_for_package(&certificate);
                false
            } else {
                demand.update_delta(&certificate, &update.added, &update.removed)
            };
            if changed {
                info!(
                    references = previous.reference_counts.len(),
                    "cluster template qualification updated ordinary source demand"
                );
            }
        } else {
            demand.update(None, BTreeSet::new());
        }
    }
}

pub(super) fn install(app: &mut App) {
    app.init_resource::<SourceFallbackDemand>();
    if let Some(render) = app.get_sub_app_mut(RenderApp) {
        render.add_systems(
            ExtractSchedule,
            publish.after(super::super::SourceExtraction),
        );
    }
}

#[cfg(test)]
#[path = "source_fallback_bridge_tests.rs"]
mod tests;
