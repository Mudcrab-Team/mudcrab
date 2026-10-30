//! Skyrim's animated shader values: the scrolling texture of a hearth's flames, a glow's pulse.
//!
//! A NIF animates a shader property's float values with a `BSEffectShaderPropertyFloatController`
//! (or its lighting-shader twin): a controlled variable - U or V offset, U or V scale, the emissive
//! multiple, alpha - driven by an `NiFloatInterpolator` over `NiFloatData` keys, looping under the
//! controller's timing. `FireplaceWood01Burning`'s two flame cards scroll their V offset by one
//! whole tile every 5.667 s and 4.267 s. Without it a
//! flame is one still, pale frame of its atlas.
//!
//! The converter publishes the controllers as `OPEN_SKYRIM_material_animation` on the material
//! with this shape:
//!
//! ```json
//! "OPEN_SKYRIM_material_animation": { "channels": [{
//!     "variable": "vOffset", "interpolation": "LINEAR" | "QUADRATIC" | "STEP",
//!     "times": [0.0, 5.6667], "values": [0.0, 1.0], "tangents": [[outgoing, incoming], ...],
//!     "loop": "cycle" | "reverse" | "clamp",
//!     "frequency": 1.0, "phase": 0.0, "start": 0.0, "stop": 5.6667 }] }
//! ```
//!
//! Static coordinates come from the loaded standard material's UV transform, including
//! `KHR_texture_transform`. Each animated coordinate replaces its static value.
//! Only scalar texture-coordinate channels have a playback consumer. Unsupported variables
//! emit one warning per variable name, including across repeated loads.
//! Material handle swaps must use component insertion: Bevy lifecycle observers cannot see
//! in-place mutation of the mutable `MeshMaterial3d` component.
//! Draw paths other than the stock material need their own UV consumer.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, OnceLock},
};

use bevy::prelude::*;

/// A shader value a channel drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variable {
    UOffset,
    VOffset,
    UScale,
    VScale,
}

impl Variable {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "uOffset" => Some(Self::UOffset),
            "vOffset" => Some(Self::VOffset),
            "uScale" => Some(Self::UScale),
            "vScale" => Some(Self::VScale),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interpolation {
    Linear,
    Quadratic,
    Step,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopMode {
    Cycle,
    Reverse,
    Clamp,
}

/// One animated value: keys, how to interpolate them, and the controller's timing.
#[derive(Debug, Clone, PartialEq)]
pub struct Channel {
    pub variable: Variable,
    pub interpolation: Interpolation,
    pub times: Vec<f32>,
    /// One scalar value per key.
    pub values: Vec<f32>,
    /// `[outgoing, incoming]` per key, for `QUADRATIC` only: a segment uses its start
    /// key's outgoing and its end key's incoming tangent, in value units per segment. NifSkope's
    /// evaluator reads them from `NiFloatData` as the start key's `Backward` and the end key's
    /// `Forward`; the converter publishes them already in this order.
    pub tangents: Vec<[f32; 2]>,
    pub loop_mode: LoopMode,
    pub frequency: f32,
    pub phase: f32,
    pub start: f32,
    pub stop: f32,
}

impl Channel {
    /// The controller's local time for a clock of `seconds`: scaled by the frequency, shifted by
    /// the phase, and brought into `start..=stop` by the loop mode.
    pub fn local_time(&self, seconds: f32) -> f32 {
        let t = seconds * self.frequency + self.phase;
        let span = self.stop - self.start;
        if !(span.is_finite() && span > 0.0 && t.is_finite()) {
            return self.start;
        }
        match self.loop_mode {
            LoopMode::Cycle => self.start + (t - self.start).rem_euclid(span),
            LoopMode::Reverse => {
                let u = (t - self.start).rem_euclid(2.0 * span);
                self.start + if u > span { 2.0 * span - u } else { u }
            }
            LoopMode::Clamp => t.clamp(self.start, self.stop),
        }
    }

    /// The channel's value at controller time `t`.
    pub fn value_at(&self, t: f32) -> f32 {
        let keys = self.times.len();
        if keys == 0 || self.values.len() != keys {
            return self.values.first().copied().unwrap_or(0.0);
        }
        let value = |key: usize| self.values[key];
        if t <= self.times[0] {
            return value(0);
        }
        if t >= self.times[keys - 1] {
            return value(keys - 1);
        }
        let i = self
            .times
            .windows(2)
            .position(|w| t >= w[0] && t <= w[1])
            .unwrap_or(0);
        let (t0, t1) = (self.times[i], self.times[i + 1]);
        let (v0, v1) = (value(i), value(i + 1));
        let s = if t1 > t0 { (t - t0) / (t1 - t0) } else { 0.0 };
        match self.interpolation {
            Interpolation::Step => v0,
            Interpolation::Linear => v0 + (v1 - v0) * s,
            Interpolation::Quadratic => {
                let m0 = self.tangents.get(i).map_or(v1 - v0, |pair| pair[0]);
                let m1 = self.tangents.get(i + 1).map_or(v1 - v0, |pair| pair[1]);
                let (s2, s3) = (s * s, s * s * s);
                (2.0 * s3 - 3.0 * s2 + 1.0) * v0
                    + (s3 - 2.0 * s2 + s) * m0
                    + (-2.0 * s3 + 3.0 * s2) * v1
                    + (s3 - s2) * m1
            }
        }
    }

    pub fn value(&self, seconds: f32) -> f32 {
        self.value_at(self.local_time(seconds))
    }
}

/// A material's animation: its channels and the static texture transform they replace.
#[derive(Debug, Clone, PartialEq)]
pub struct MaterialAnimation {
    pub channels: Vec<Channel>,
    pub base_offset: Vec2,
    pub base_scale: Vec2,
    base_rotation: f32,
    base_matrix: bevy::math::Mat2,
}

impl MaterialAnimation {
    /// The texture offset and scale at `seconds`: each animated coordinate replaces its static one.
    pub fn uv_offset_scale(&self, seconds: f32) -> (Vec2, Vec2) {
        let (mut offset, mut scale) = (self.base_offset, self.base_scale);
        for channel in &self.channels {
            let value = channel.value(seconds);
            match channel.variable {
                Variable::UOffset => offset.x = value,
                Variable::VOffset => offset.y = value,
                Variable::UScale => scale.x = value,
                Variable::VScale => scale.y = value,
            }
        }
        (offset, scale)
    }

    /// Preserve the loaded material's rotation and every unanimated UV coordinate.
    pub fn transform_at(&self, seconds: f32) -> bevy::math::Affine2 {
        let (offset, scale) = self.uv_offset_scale(seconds);
        let mut matrix = self.base_matrix;
        let rotation = bevy::math::Mat2::from_angle(self.base_rotation);
        for channel in &self.channels {
            match channel.variable {
                Variable::UScale => matrix.x_axis = rotation.x_axis * scale.x,
                Variable::VScale => matrix.y_axis = rotation.y_axis * scale.y,
                Variable::UOffset | Variable::VOffset => {}
            }
        }
        bevy::math::Affine2::from_mat2_translation(matrix, offset)
    }

    fn capture_base(&mut self, transform: bevy::math::Affine2) {
        // Affine2 decomposition requires a nonzero determinant. Zero texture
        // scales are valid; retain their exact matrix and derive the surviving axis.
        let (scale, rotation, offset) = if transform.matrix2.determinant() != 0.0 {
            transform.to_scale_angle_translation()
        } else {
            let matrix = transform.matrix2;
            let rotation = if matrix.y_axis != Vec2::ZERO {
                (-matrix.y_axis.x).atan2(matrix.y_axis.y)
            } else {
                matrix.x_axis.y.atan2(matrix.x_axis.x)
            };
            (
                Vec2::new(matrix.x_axis.length(), matrix.y_axis.length()),
                rotation,
                transform.translation,
            )
        };
        self.base_matrix = transform.matrix2;
        self.base_offset = offset;
        self.base_scale = scale;
        self.base_rotation = rotation;
    }

    /// Parse supported scalar UV channels and retain the loader's static transform.
    pub fn from_extensions(
        animation: &serde_json::Value,
        base: bevy::math::Affine2,
    ) -> Option<Self> {
        let channels: Vec<Channel> = animation
            .get("channels")?
            .as_array()?
            .iter()
            .filter_map(parse_channel)
            .collect();
        if channels.is_empty() {
            return None;
        }
        let mut animation = Self {
            channels,
            base_offset: Vec2::ZERO,
            base_scale: Vec2::ONE,
            base_rotation: 0.0,
            base_matrix: bevy::math::Mat2::IDENTITY,
        };
        animation.capture_base(base);
        Some(animation)
    }
}

fn parse_channel(value: &serde_json::Value) -> Option<Channel> {
    let name = value.get("variable")?.as_str()?;
    let Some(variable) = Variable::parse(name) else {
        static WARNED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
        if let Ok(mut warned) = WARNED.get_or_init(Mutex::default).lock()
            && warned.insert(name.to_owned())
        {
            warn!(
                variable = name,
                "unsupported material animation variable; channel ignored"
            );
        }
        return None;
    };
    let floats = |key: &str| -> Option<Vec<f32>> {
        value
            .get(key)?
            .as_array()?
            .iter()
            .map(|entry| entry.as_f64().map(|v| v as f32))
            .collect()
    };
    let number = |key: &str, default: f32| {
        value
            .get(key)
            .and_then(|entry| entry.as_f64())
            .map_or(default, |v| v as f32)
    };
    let times = floats("times")?;
    let values = floats("values")?;
    if times.is_empty()
        || times.len() != values.len()
        || value
            .get("components")
            .is_some_and(|count| count.as_u64() != Some(1))
    {
        return None;
    }
    let interpolation = match value.get("interpolation").and_then(|v| v.as_str()) {
        Some("QUADRATIC") => Interpolation::Quadratic,
        Some("STEP") => Interpolation::Step,
        _ => Interpolation::Linear,
    };
    let tangents = value
        .get("tangents")
        .and_then(|v| v.as_array())
        .map(|pairs| {
            pairs
                .iter()
                .filter_map(|pair| {
                    let pair = pair.as_array()?;
                    Some([
                        pair.first()?.as_f64()? as f32,
                        pair.get(1)?.as_f64()? as f32,
                    ])
                })
                .collect()
        })
        .unwrap_or_default();
    let loop_mode = match value.get("loop").and_then(|v| v.as_str()) {
        Some("reverse") => LoopMode::Reverse,
        Some("clamp") => LoopMode::Clamp,
        _ => LoopMode::Cycle,
    };
    let start = number("start", times[0]);
    let stop = number("stop", times[times.len() - 1]);
    Some(Channel {
        variable,
        interpolation,
        times,
        values,
        tangents,
        loop_mode,
        frequency: number("frequency", 1.0),
        phase: number("phase", 0.0),
        start,
        stop,
    })
}

/// Animation payloads recorded by the glTF handler, keyed by standard-material asset path.
/// Loader tasks share this registry without retaining otherwise unused labelled assets.
#[derive(Resource, Clone, Default)]
pub struct MaterialAnimationRegistry(Arc<Mutex<MaterialAnimationPayloads>>);

#[derive(Default)]
struct MaterialAnimationPayloads {
    by_source: HashMap<String, HashMap<String, MaterialAnimation>>,
    changed: HashSet<String>,
}

impl MaterialAnimationRegistry {
    pub fn insert(&self, material_path: String, animation: MaterialAnimation) {
        self.replace(&material_path, Some(animation));
    }

    pub(crate) fn replace(&self, material_path: &str, animation: Option<MaterialAnimation>) {
        let (source, label) = material_path
            .rsplit_once('#')
            .unwrap_or(("", material_path));
        if let Ok(mut state) = self.0.lock() {
            let MaterialAnimationPayloads {
                by_source: animations,
                changed,
            } = &mut *state;
            if let Some(animation) = animation {
                animations
                    .entry(source.to_owned())
                    .or_default()
                    .insert(label.to_owned(), animation);
                changed.insert(material_path.to_owned());
            } else if let Some(labels) = animations.get_mut(source) {
                if labels.remove(label).is_some() {
                    changed.insert(material_path.to_owned());
                }
                if labels.is_empty() {
                    animations.remove(source);
                }
            }
        }
    }

    pub fn get(&self, material_path: &str) -> Option<MaterialAnimation> {
        let (source, label) = material_path
            .rsplit_once('#')
            .unwrap_or(("", material_path));
        self.0
            .lock()
            .ok()?
            .by_source
            .get(source)?
            .get(label)
            .cloned()
    }

    /// Withdraw this handler's labels before rebuilding one source, including labels that vanish.
    /// Index by source so a model reload does not scan every loaded model's materials.
    pub(crate) fn clear_source(&self, source: &str, suffix: &str) {
        if let Ok(mut state) = self.0.lock() {
            let MaterialAnimationPayloads { by_source, changed } = &mut *state;
            if let Some(labels) = by_source.get_mut(source) {
                labels.retain(|label, _| {
                    if label.ends_with(suffix) {
                        changed.insert(format!("{source}#{label}"));
                        false
                    } else {
                        true
                    }
                });
                if labels.is_empty() {
                    by_source.remove(source);
                }
            }
        }
    }

    fn take_changes(&self) -> HashSet<String> {
        self.0
            .lock()
            .map(|mut state| std::mem::take(&mut state.changed))
            .unwrap_or_default()
    }
}

/// Asset IDs only: the registry must not keep unused materials alive.
#[derive(Resource, Default)]
pub(crate) struct AnimatedStandardMaterials {
    resolved: HashSet<AssetId<StandardMaterial>>,
    animated: HashMap<AssetId<StandardMaterial>, MaterialAnimation>,
    /// Registration/removal events, never a per-frame component change query.
    dirty_users: HashSet<AssetId<StandardMaterial>>,
    /// One tracked write per material per playback call. AssetEventSystems
    /// flushes it before the next resolver call; extra Modified events are external.
    own_modified: HashSet<AssetId<StandardMaterial>>,
    /// Pathless variants retain the authored source animation, including its static UV base.
    derived_sources: HashMap<AssetId<StandardMaterial>, AssetId<StandardMaterial>>,
    derived_by_source: HashMap<AssetId<StandardMaterial>, HashSet<AssetId<StandardMaterial>>>,
}

impl AnimatedStandardMaterials {
    /// Associate an emittance variant without retaining either asset's handle.
    pub(crate) fn derive(
        &mut self,
        source: AssetId<StandardMaterial>,
        derived: AssetId<StandardMaterial>,
    ) {
        if let Some(previous) = self.derived_sources.insert(derived, source)
            && let Some(variants) = self.derived_by_source.get_mut(&previous)
        {
            variants.remove(&derived);
            if variants.is_empty() {
                self.derived_by_source.remove(&previous);
            }
        }
        self.derived_by_source
            .entry(source)
            .or_default()
            .insert(derived);
        self.refresh_derived_material(source, derived);
    }

    fn refresh_derived(&mut self, source: AssetId<StandardMaterial>) {
        // Move the set while updating sibling fields, retaining its allocation.
        let Some(variants) = self.derived_by_source.remove(&source) else {
            return;
        };
        for &derived in &variants {
            self.refresh_derived_material(source, derived);
        }
        self.derived_by_source.insert(source, variants);
    }

    fn refresh_derived_material(
        &mut self,
        source: AssetId<StandardMaterial>,
        derived: AssetId<StandardMaterial>,
    ) {
        self.resolved.remove(&derived);
        self.animated.remove(&derived);
        if let Some(animation) = self.animated.get(&source).cloned() {
            self.animated.insert(derived, animation);
            self.resolved.insert(derived);
        }
        self.dirty_users.insert(derived);
    }

    fn register(
        &mut self,
        id: AssetId<StandardMaterial>,
        path: Option<&str>,
        material: Option<&StandardMaterial>,
        registry: Option<&MaterialAnimationRegistry>,
    ) {
        if let Some(&source) = self.derived_sources.get(&id) {
            self.refresh_derived_material(source, id);
            return;
        }
        // Withdraw the old answer before resolving readiness or a genuine modification.
        self.resolved.remove(&id);
        self.animated.remove(&id);
        self.dirty_users.insert(id);
        let (Some(path), Some(material), Some(registry)) = (path, material, registry) else {
            self.refresh_derived(id);
            return;
        };
        if let Some(mut animation) = registry.get(path) {
            // StandardMaterial is authoritative after Bevy interprets texture infos.
            animation.capture_base(material.uv_transform);
            self.animated.insert(id, animation);
        }
        self.resolved.insert(id);
        self.refresh_derived(id);
    }

    fn forget(&mut self, id: AssetId<StandardMaterial>) {
        self.resolved.remove(&id);
        self.animated.remove(&id);
        self.own_modified.remove(&id);
        self.dirty_users.insert(id);
        if let Some(source) = self.derived_sources.remove(&id)
            && let Some(variants) = self.derived_by_source.get_mut(&source)
        {
            variants.remove(&id);
            if variants.is_empty() {
                self.derived_by_source.remove(&source);
            }
        }
        self.refresh_derived(id);
    }
}

/// Only animated component users are retained. New registrations find existing users
/// in one event-driven scan; settled frames do not visit unrelated entities.
#[derive(Resource, Default)]
struct MaterialUsers {
    entities: HashMap<Entity, AssetId<StandardMaterial>>,
    materials: HashMap<AssetId<StandardMaterial>, HashSet<Entity>>,
    /// Every entity examined by lifecycle indexing or registration scans.
    #[cfg(test)]
    entity_visits: usize,
}

impl MaterialUsers {
    fn insert(&mut self, entity: Entity, id: AssetId<StandardMaterial>) {
        self.entities.insert(entity, id);
        self.materials.entry(id).or_default().insert(entity);
    }

    fn remove_material(&mut self, id: AssetId<StandardMaterial>) {
        if let Some(users) = self.materials.remove(&id) {
            for entity in users {
                #[cfg(test)]
                {
                    self.entity_visits += 1;
                }
                self.entities.remove(&entity);
            }
        }
    }

    fn remove(&mut self, entity: Entity) {
        if let Some(id) = self.entities.remove(&entity)
            && let Some(users) = self.materials.get_mut(&id)
        {
            users.remove(&entity);
            if users.is_empty() {
                self.materials.remove(&id);
            }
        }
    }
}

pub struct MaterialAnimationPlugin;

impl Plugin for MaterialAnimationPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AnimatedStandardMaterials>()
            .init_resource::<MaterialUsers>()
            .add_observer(on_material_insert)
            .add_observer(on_material_discard)
            .add_systems(
                PostUpdate,
                (
                    find_animated_standard_materials,
                    index_material_users,
                    animate_materials,
                )
                    .chain()
                    .after(bevy::asset::AssetEventSystems)
                    .after(bevy::camera::visibility::VisibilitySystems::CheckVisibility),
            );
    }
}

fn on_material_insert(
    event: On<Insert, MeshMaterial3d<StandardMaterial>>,
    materials: Query<&MeshMaterial3d<StandardMaterial>>,
    animated: Res<AnimatedStandardMaterials>,
    mut index: ResMut<MaterialUsers>,
) {
    #[cfg(test)]
    {
        index.entity_visits += 1;
    }
    if let Ok(material) = materials.get(event.entity)
        && animated.animated.contains_key(&material.id())
    {
        index.insert(event.entity, material.id());
    }
}

fn on_material_discard(
    event: On<Discard, MeshMaterial3d<StandardMaterial>>,
    mut index: ResMut<MaterialUsers>,
) {
    #[cfg(test)]
    {
        index.entity_visits += 1;
    }
    index.remove(event.entity);
}

fn index_material_users(
    materials: Query<(Entity, &MeshMaterial3d<StandardMaterial>)>,
    mut animated: ResMut<AnimatedStandardMaterials>,
    mut index: ResMut<MaterialUsers>,
) {
    if animated.dirty_users.is_empty() {
        return;
    }
    let mut dirty = std::mem::take(&mut animated.dirty_users);
    for id in &dirty {
        index.remove_material(*id);
    }
    dirty.retain(|id| animated.animated.contains_key(id));
    if dirty.is_empty() {
        return;
    }
    // Late registration can follow entity insertion. One scan per real event
    // batch finds those users; there is no settled-frame component iteration.
    for (entity, material) in &materials {
        #[cfg(test)]
        {
            index.entity_visits += 1;
        }
        if dirty.contains(&material.id()) {
            index.insert(entity, material.id());
        }
    }
}

/// Resolve only after both the asset path and the loaded material exist. A later
/// LoadedWithDependencies event retries an Added event that arrived too early.
fn find_animated_standard_materials(
    mut events: MessageReader<AssetEvent<StandardMaterial>>,
    asset_server: Res<AssetServer>,
    standard: Res<Assets<StandardMaterial>>,
    registry: Option<Res<MaterialAnimationRegistry>>,
    mut animated: ResMut<AnimatedStandardMaterials>,
) {
    if let Some(registry) = registry.as_deref() {
        // Omitted labels do not emit an asset event while a strong handle survives.
        // Resolve only journaled paths; empty settled frames visit no assets or entities.
        for path in registry.take_changes() {
            if let Some(handle) = asset_server.get_handle::<StandardMaterial>(path.clone()) {
                animated.register(
                    handle.id(),
                    Some(&path),
                    standard.get(handle.id()),
                    Some(registry),
                );
            }
        }
    }
    for event in events.read() {
        match event {
            AssetEvent::Removed { id } | AssetEvent::Unused { id } => animated.forget(*id),
            AssetEvent::Modified { id } if animated.own_modified.remove(id) => {
                // Consume one event, not every modification of this asset. If
                // another writer/reload changed it too, its additional Modified
                // (or LoadedWithDependencies) below still forces resolution.
            }
            AssetEvent::Added { id }
            | AssetEvent::LoadedWithDependencies { id }
            | AssetEvent::Modified { id } => {
                let path = asset_server.get_path(*id).map(|path| path.to_string());
                animated.register(*id, path.as_deref(), standard.get(*id), registry.as_deref());
            }
        }
    }
}

/// Touch only animated assets and their indexed users. Invisible materials retain
/// their last value and catch up to the clock when they become visible.
fn animate_materials(
    time: Res<Time>,
    mut standard: ResMut<Assets<StandardMaterial>>,
    mut animated: ResMut<AnimatedStandardMaterials>,
    index: Res<MaterialUsers>,
    visibility: Query<&ViewVisibility>,
    cluster: Option<Res<crate::cluster::render::ClusterBatchHandles>>,
    mut clustered: Local<HashSet<AssetId<StandardMaterial>>>,
) {
    if let Some(cluster) = cluster.as_ref() {
        if cluster.is_changed() {
            clustered.clear();
            clustered.extend(
                cluster
                    .ids()
                    .filter_map(|id| id.try_typed::<StandardMaterial>().ok()),
            );
        }
    } else {
        clustered.clear();
    }
    let AnimatedStandardMaterials {
        animated,
        own_modified,
        ..
    } = &mut *animated;
    for (id, animation) in animated.iter() {
        let transform = animation.transform_at(time.elapsed_secs());
        let Some(material) = standard.get(*id) else {
            continue;
        };
        // Check before visiting users and before acquiring a mutable asset.
        // Paused clocks, completed Clamp, STEP plateaus and constant channels
        // neither emit Modified nor visit entity visibility while settled.
        if material.uv_transform == transform {
            continue;
        }
        let visible = index.materials.get(id).is_some_and(|users| {
            users
                .iter()
                .any(|entity| visibility.get(*entity).is_ok_and(|v| v.get()))
        });
        if !visible && !clustered.contains(id) {
            continue;
        }
        if let Some(mut material) = standard.get_mut(*id) {
            material.uv_transform = transform;
            own_modified.insert(*id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emittance_variants_keep_source_uv_animation_base_and_lifetime() {
        use crate::{
            emittance::{EmittancePlugin, ReferenceEmittance},
            nif_material::ExternalEmittance,
        };
        use bevy::{asset::AssetApp, time::TimeUpdateStrategy};
        use std::time::Duration;
        for late in [false, true] {
            let mut app = App::new();
            app.add_plugins((MinimalPlugins, AssetPlugin::default()))
                .init_asset::<StandardMaterial>()
                .add_plugins((EmittancePlugin, MaterialAnimationPlugin))
                .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(1)));
            let base = bevy::math::Affine2::from_scale_angle_translation(
                Vec2::new(2.0, 3.0),
                0.2,
                Vec2::new(0.25, 0.75),
            );
            let source = app
                .world_mut()
                .resource_mut::<Assets<StandardMaterial>>()
                .add(StandardMaterial {
                    uv_transform: base,
                    emissive: LinearRgba::new(2.0, 3.0, 4.0, 1.0),
                    ..default()
                });
            app.update(); // Consume the pathless test source's Added event before injecting its decoded controller.
            let animation = MaterialAnimation::from_extensions(&flame(), base).unwrap();
            let registry = MaterialAnimationRegistry::default();
            let path = "emittance.glb#Material0/nif";
            registry.insert(path.into(), animation.clone());
            let register = |app: &mut App| {
                let material = app
                    .world()
                    .resource::<Assets<StandardMaterial>>()
                    .get(&source)
                    .unwrap()
                    .clone();
                app.world_mut()
                    .resource_mut::<AnimatedStandardMaterials>()
                    .register(source.id(), Some(path), Some(&material), Some(&registry));
            };
            if !late {
                register(&mut app);
            }
            let mut surfaces = Vec::new();
            for colour in [[255, 0, 0], [0, 255, 0], [255, 0, 0]] {
                let surface = app
                    .world_mut()
                    .spawn((
                        MeshMaterial3d(source.clone()),
                        ExternalEmittance,
                        ViewVisibility::VISIBLE,
                    ))
                    .id();
                app.world_mut()
                    .spawn(ReferenceEmittance::from_srgb(colour))
                    .add_child(surface);
                surfaces.push(surface);
            }
            app.update();
            let variants: Vec<_> = surfaces
                .iter()
                .map(|&surface| {
                    app.world()
                        .get::<MeshMaterial3d<StandardMaterial>>(surface)
                        .unwrap()
                        .0
                        .clone()
                })
                .collect();
            assert_eq!(
                variants[0], variants[2],
                "equal colours retain material batching"
            );
            assert_ne!(variants[0], variants[1]);
            assert_ne!(variants[0], source);
            assert!(
                app.world()
                    .resource::<AssetServer>()
                    .get_path(variants[0].id())
                    .is_none(),
                "regression requires a pathless clone"
            );
            if late {
                register(&mut app);
            }
            for _ in 0..4 {
                app.update();
                let seconds = app.world().resource::<Time>().elapsed_secs();
                let materials = app.world().resource::<Assets<StandardMaterial>>();
                for variant in &variants {
                    assert_eq!(
                        materials.get(variant).unwrap().uv_transform,
                        animation.transform_at(seconds),
                        "derived animation must retain the source's static UV base (late: {late})"
                    );
                }
                assert_eq!(
                    materials.get(&variants[0]).unwrap().emissive,
                    LinearRgba::new(2.0, 0.0, 0.0, 1.0)
                );
                assert_eq!(
                    materials.get(&variants[1]).unwrap().emissive,
                    LinearRgba::new(0.0, 3.0, 0.0, 1.0)
                );
            }
            registry.replace(path, None);
            register(&mut app);
            let stopped = app
                .world()
                .resource::<Assets<StandardMaterial>>()
                .get(&variants[0])
                .unwrap()
                .uv_transform;
            for _ in 0..3 {
                app.update();
                assert_eq!(
                    app.world()
                        .resource::<Assets<StandardMaterial>>()
                        .get(&variants[0])
                        .unwrap()
                        .uv_transform,
                    stopped,
                    "source withdrawal must stop retained variants"
                );
            }
            let removed = variants[0].id();
            app.world_mut()
                .resource_mut::<Assets<StandardMaterial>>()
                .remove(removed);
            for _ in 0..3 {
                app.update();
            }
            let animations = app.world().resource::<AnimatedStandardMaterials>();
            assert!(!animations.derived_sources.contains_key(&removed));
            assert!(
                !animations
                    .derived_by_source
                    .get(&source.id())
                    .is_some_and(|variants| variants.contains(&removed))
            );
        }
    }

    #[test]
    fn removing_or_rebinding_cluster_users_stops_writing_an_undrawn_animation() {
        use crate::cluster::render::ClusterBatchHandles;
        for remove in [false, true] {
            let mut app = App::new();
            app.init_resource::<Time>()
                .init_resource::<Assets<StandardMaterial>>()
                .init_resource::<AnimatedStandardMaterials>()
                .init_resource::<MaterialUsers>()
                .init_resource::<ClusterBatchHandles>()
                .add_observer(on_material_insert)
                .add_observer(on_material_discard)
                .add_systems(Update, (index_material_users, animate_materials).chain());
            let animated = app
                .world_mut()
                .resource_mut::<Assets<StandardMaterial>>()
                .add(StandardMaterial::default());
            let unanimated = app
                .world_mut()
                .resource_mut::<Assets<StandardMaterial>>()
                .add(StandardMaterial::default());
            app.world_mut()
                .resource_mut::<AnimatedStandardMaterials>()
                .animated
                .insert(
                    animated.id(),
                    MaterialAnimation::from_extensions(&flame(), bevy::math::Affine2::IDENTITY)
                        .unwrap(),
                );
            app.world_mut()
                .resource_mut::<ClusterBatchHandles>()
                .set(0, animated.clone());
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs(1));
            app.update();
            let played = app
                .world()
                .resource::<Assets<StandardMaterial>>()
                .get(&animated)
                .unwrap()
                .uv_transform;
            assert_ne!(played, bevy::math::Affine2::IDENTITY);
            if remove {
                app.world_mut().remove_resource::<ClusterBatchHandles>();
            } else {
                app.world_mut()
                    .resource_mut::<ClusterBatchHandles>()
                    .set(0, unanimated.clone());
            }
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs(1));
            app.update();
            let materials = app.world().resource::<Assets<StandardMaterial>>();
            assert_eq!(
                materials.get(&animated).unwrap().uv_transform,
                played,
                "undrawn animated material must stop receiving writes (removed: {remove})"
            );
            assert_eq!(
                materials.get(&unanimated).unwrap().uv_transform,
                bevy::math::Affine2::IDENTITY
            );
        }
    }

    #[test]
    fn stock_visibility_and_material_rebinding_clear_previous_drawn_users() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<Assets<StandardMaterial>>()
            .init_resource::<AnimatedStandardMaterials>()
            .init_resource::<MaterialUsers>()
            .add_observer(on_material_insert)
            .add_observer(on_material_discard)
            .add_systems(Update, (index_material_users, animate_materials).chain());
        let animated = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial::default());
        let unanimated = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial::default());
        app.world_mut()
            .resource_mut::<AnimatedStandardMaterials>()
            .animated
            .insert(
                animated.id(),
                MaterialAnimation::from_extensions(&flame(), bevy::math::Affine2::IDENTITY)
                    .unwrap(),
            );
        let visible = ViewVisibility::VISIBLE;
        let user = app
            .world_mut()
            .spawn((MeshMaterial3d(animated.clone()), visible))
            .id();
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs(1));
        app.update();
        let played = app
            .world()
            .resource::<Assets<StandardMaterial>>()
            .get(&animated)
            .unwrap()
            .uv_transform;
        app.world_mut()
            .entity_mut(user)
            .insert(ViewVisibility::default());
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs(1));
        app.update();
        assert_eq!(
            app.world()
                .resource::<Assets<StandardMaterial>>()
                .get(&animated)
                .unwrap()
                .uv_transform,
            played
        );
        app.world_mut()
            .entity_mut(user)
            .insert((MeshMaterial3d(unanimated.clone()), visible));
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs(1));
        app.update();
        let materials = app.world().resource::<Assets<StandardMaterial>>();
        assert_eq!(materials.get(&animated).unwrap().uv_transform, played);
        assert_eq!(
            materials.get(&unanimated).unwrap().uv_transform,
            bevy::math::Affine2::IDENTITY
        );
    }

    /// `FireplaceWood01Burning`'s first flame card: V offset, one tile
    /// every 5.667 s, cycling.
    fn flame() -> serde_json::Value {
        serde_json::json!({"channels": [{
            "variable": "vOffset", "interpolation": "LINEAR",
            "times": [0.0, 5.6667], "values": [0.0, 1.0],
            "loop": "cycle", "frequency": 1.0, "phase": 0.0, "start": 0.0, "stop": 5.6667
        }]})
    }

    #[test]
    fn a_flame_scrolls_one_tile_per_loop_and_wraps() {
        let animation =
            MaterialAnimation::from_extensions(&flame(), bevy::math::Affine2::IDENTITY).unwrap();
        let (offset, scale) = animation.uv_offset_scale(0.0);
        assert_eq!((offset, scale), (Vec2::ZERO, Vec2::ONE));
        let (offset, _) = animation.uv_offset_scale(5.6667 / 2.0);
        assert!(
            (offset.y - 0.5).abs() < 1e-4,
            "halfway through the loop, {offset}"
        );
        assert_eq!(offset.x, 0.0, "only V is animated");
        let (offset, _) = animation.uv_offset_scale(5.6667 + 1.0);
        let (again, _) = animation.uv_offset_scale(1.0);
        assert!((offset.y - again.y).abs() < 1e-4, "a cycle repeats");
    }

    #[test]
    fn reverse_plays_back_and_clamp_holds() {
        let mut channel = parse_channel(&flame()["channels"][0]).unwrap();
        channel.loop_mode = LoopMode::Reverse;
        assert!(
            (channel.value(5.6667 * 1.25) - 0.75).abs() < 1e-3,
            "on the way back"
        );
        channel.loop_mode = LoopMode::Clamp;
        assert!((channel.value(100.0) - 1.0).abs() < 1e-6, "held at the end");
        assert_eq!(channel.value(-3.0), 0.0, "and at the start");
    }

    #[test]
    fn step_and_quadratic_keys() {
        let mut channel = parse_channel(&serde_json::json!({
            "variable": "uScale", "interpolation": "STEP",
            "times": [0.0, 1.0, 2.0], "values": [1.0, 2.0, 3.0]
        }))
        .unwrap();
        assert_eq!(channel.value_at(0.5), 1.0);
        assert_eq!(channel.value_at(1.5), 2.0);
        channel.interpolation = Interpolation::Quadratic;
        channel.tangents = vec![[1.0, 1.0]; 3];
        // With tangents equal to the slope, Hermite is the straight line.
        assert!((channel.value_at(0.5) - 1.5).abs() < 1e-5);
    }

    #[test]
    fn static_scale_and_rotation_survive_an_offset_channel() {
        let base = bevy::math::Affine2::from_scale_angle_translation(
            Vec2::splat(2.0),
            0.3,
            Vec2::new(0.25, 0.5),
        );
        let animation = MaterialAnimation::from_extensions(&flame(), base).unwrap();
        let (offset, scale) = animation.uv_offset_scale(0.0);
        assert_eq!(offset, Vec2::new(0.25, 0.0));
        assert!((scale - Vec2::splat(2.0)).length() < 1e-5);
        let expected = bevy::math::Affine2::from_scale_angle_translation(scale, 0.3, offset);
        assert!(
            (animation.transform_at(0.0).matrix2 - expected.matrix2)
                .abs_diff_eq(bevy::math::Mat2::ZERO, 1e-5)
        );
    }

    #[test]
    fn unsupported_variables_and_vector_channels_do_not_register() {
        for variable in ["emissiveColor", "emissiveMultiple", "alpha"] {
            let payload = serde_json::json!({"channels": [{
                "variable": variable, "times": [0.0, 1.0], "values": [0.0, 1.0]
            }]});
            assert!(
                MaterialAnimation::from_extensions(&payload, bevy::math::Affine2::IDENTITY)
                    .is_none()
            );
        }
        assert!(
            parse_channel(&serde_json::json!({
                "variable": "uOffset", "components": 3,
                "times": [0.0], "values": [0.0, 1.0, 2.0]
            }))
            .is_none()
        );
    }
    #[test]
    fn registration_retries_missing_path_and_uses_the_loaded_standard_uv_base() {
        let mut assets = Assets::<StandardMaterial>::default();
        let handle = assets.add(StandardMaterial {
            uv_transform: bevy::math::Affine2::from_scale_angle_translation(
                Vec2::splat(2.0),
                0.0,
                Vec2::new(0.25, 0.5),
            ),
            ..default()
        });
        let registry = MaterialAnimationRegistry::default();
        let path = "animated.glb#Material0/std";
        registry.insert(
            path.into(),
            MaterialAnimation::from_extensions(&flame(), bevy::math::Affine2::IDENTITY).unwrap(),
        );
        let mut index = AnimatedStandardMaterials::default();
        index.register(handle.id(), None, assets.get(&handle), Some(&registry));
        assert!(index.resolved.is_empty() && index.animated.is_empty());
        index.register(handle.id(), Some(path), None, Some(&registry));
        assert!(index.resolved.is_empty());
        index.register(
            handle.id(),
            Some(path),
            assets.get(&handle),
            Some(&registry),
        );
        let animation = &index.animated[&handle.id()];
        let (offset, scale) = animation.uv_offset_scale(0.0);
        assert_eq!(offset, Vec2::new(0.25, 0.0));
        assert_eq!(scale, Vec2::splat(2.0));
        assert!(index.resolved.contains(&handle.id()));
    }

    #[test]
    fn removed_and_unused_asset_events_release_all_resolution_state() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<StandardMaterial>()
            .init_resource::<AnimatedStandardMaterials>()
            .add_systems(Update, find_animated_standard_materials);
        for unused in [false, true] {
            let handle = app
                .world_mut()
                .resource_mut::<Assets<StandardMaterial>>()
                .add(StandardMaterial::default());
            let id = handle.id();
            {
                let mut index = app.world_mut().resource_mut::<AnimatedStandardMaterials>();
                index.resolved.insert(id);
                index.animated.insert(
                    id,
                    MaterialAnimation::from_extensions(&flame(), bevy::math::Affine2::IDENTITY)
                        .unwrap(),
                );
            }
            app.world_mut().write_message(if unused {
                AssetEvent::<StandardMaterial>::Unused { id }
            } else {
                AssetEvent::<StandardMaterial>::Removed { id }
            });
            app.update();
            let index = app.world().resource::<AnimatedStandardMaterials>();
            assert!(!index.resolved.contains(&id) && !index.animated.contains_key(&id));
        }
    }

    #[test]
    fn removing_material_components_and_despawning_users_cleans_the_inverse_index() {
        let mut app = App::new();
        app.init_resource::<Assets<StandardMaterial>>()
            .init_resource::<MaterialUsers>()
            .init_resource::<AnimatedStandardMaterials>()
            .add_observer(on_material_insert)
            .add_observer(on_material_discard)
            .add_systems(Update, index_material_users);
        let handle = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial::default());
        register_fixture(&mut app, &handle, &flame());
        let removed = app.world_mut().spawn(MeshMaterial3d(handle.clone())).id();
        let despawned = app.world_mut().spawn(MeshMaterial3d(handle.clone())).id();
        app.update();
        assert_eq!(
            app.world().resource::<MaterialUsers>().materials[&handle.id()].len(),
            2
        );
        app.world_mut()
            .entity_mut(removed)
            .remove::<MeshMaterial3d<StandardMaterial>>();
        app.world_mut().entity_mut(despawned).despawn();
        app.update();
        let index = app.world().resource::<MaterialUsers>();
        assert!(index.entities.is_empty() && index.materials.is_empty());
    }

    #[test]
    fn an_animation_registered_after_its_users_are_indexed_starts_without_rebinding() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<Assets<StandardMaterial>>()
            .init_resource::<MaterialUsers>()
            .init_resource::<AnimatedStandardMaterials>()
            .add_observer(on_material_insert)
            .add_observer(on_material_discard)
            .add_systems(Update, (index_material_users, animate_materials).chain());
        let handle = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial::default());
        app.world_mut()
            .spawn((MeshMaterial3d(handle.clone()), ViewVisibility::VISIBLE));
        app.update();
        register_fixture(&mut app, &handle, &flame());
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs(1));
        app.update();
        assert_ne!(
            app.world()
                .resource::<Assets<StandardMaterial>>()
                .get(&handle)
                .unwrap()
                .uv_transform,
            bevy::math::Affine2::IDENTITY
        );
    }
    #[test]
    fn an_offset_animation_preserves_a_singular_static_texture_matrix() {
        let base = bevy::math::Affine2::from_scale_angle_translation(
            Vec2::new(0.0, 2.0),
            0.3,
            Vec2::new(0.25, 0.5),
        );
        let animation = MaterialAnimation::from_extensions(&flame(), base).unwrap();
        assert_eq!(animation.transform_at(0.0).matrix2, base.matrix2);
        assert_eq!(
            animation.transform_at(0.0).translation,
            Vec2::new(0.25, 0.0)
        );
    }
    fn observed_app() -> App {
        let mut app = App::new();
        app.init_resource::<Time>()
            .add_plugins(AssetPlugin::default())
            .init_asset::<StandardMaterial>()
            .add_plugins(MaterialAnimationPlugin);
        app
    }

    fn register_fixture(
        app: &mut App,
        handle: &Handle<StandardMaterial>,
        payload: &serde_json::Value,
    ) {
        let registry = MaterialAnimationRegistry::default();
        registry.insert(
            "fixture".into(),
            MaterialAnimation::from_extensions(payload, bevy::math::Affine2::IDENTITY).unwrap(),
        );
        let material = app
            .world()
            .resource::<Assets<StandardMaterial>>()
            .get(handle)
            .unwrap()
            .clone();
        app.world_mut()
            .resource_mut::<AnimatedStandardMaterials>()
            .register(
                handle.id(),
                Some("fixture"),
                Some(&material),
                Some(&registry),
            );
    }

    fn modified_events(
        app: &App,
        cursor: &mut bevy::ecs::message::MessageCursor<AssetEvent<StandardMaterial>>,
    ) -> usize {
        cursor
            .read(
                app.world()
                    .resource::<Messages<AssetEvent<StandardMaterial>>>(),
            )
            .filter(|event| matches!(event, AssetEvent::Modified { .. }))
            .count()
    }

    #[test]
    fn settled_uvs_emit_no_modified_events_but_real_changes_do() {
        // Unconditional UV writes fail each settled case; suppressing all writes fails the controls.
        for case in ["paused", "clamp", "step", "constant"] {
            let mut app = observed_app();
            let handle = app
                .world_mut()
                .resource_mut::<Assets<StandardMaterial>>()
                .add(StandardMaterial::default());
            app.update(); // Drain Added before installing a pathless fixture.
            let mut payload = flame();
            payload["channels"][0]["times"] = serde_json::json!([0.0, 1.0, 2.0]);
            payload["channels"][0]["values"] = serde_json::json!([0.0, 1.0, 2.0]);
            payload["channels"][0]["stop"] = serde_json::json!(2.0);
            match case {
                "clamp" => payload["channels"][0]["loop"] = serde_json::json!("clamp"),
                "step" => payload["channels"][0]["interpolation"] = serde_json::json!("STEP"),
                "constant" => payload["channels"][0]["values"] = serde_json::json!([0.5, 0.5, 0.5]),
                _ => {}
            }
            register_fixture(&mut app, &handle, &payload);
            app.world_mut()
                .spawn((MeshMaterial3d(handle.clone()), ViewVisibility::VISIBLE));
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(if case == "clamp" {
                    3.0
                } else {
                    0.25
                }));
            app.update();
            app.update(); // Flush the first playback write through real Bevy asset events.
            let expected = match case {
                "clamp" => 2.0,
                "step" => 0.0,
                "constant" => 0.5,
                _ => 0.25,
            };
            assert_eq!(
                app.world()
                    .resource::<Assets<StandardMaterial>>()
                    .get(&handle)
                    .unwrap()
                    .uv_transform
                    .translation,
                Vec2::new(0.0, expected)
            );
            assert!(
                app.world()
                    .resource::<AnimatedStandardMaterials>()
                    .animated
                    .contains_key(&handle.id())
            );
            let mut cursor = app
                .world()
                .resource::<Messages<AssetEvent<StandardMaterial>>>()
                .get_cursor_current();
            let work = app.world().resource::<MaterialUsers>().entity_visits;
            for _ in 0..4 {
                if case != "paused" {
                    app.world_mut()
                        .resource_mut::<Time>()
                        .advance_by(std::time::Duration::from_millis(50));
                }
                app.update();
                assert_eq!(modified_events(&app, &mut cursor), 0, "settled {case}");
            }
            assert_eq!(
                app.world().resource::<MaterialUsers>().entity_visits,
                work,
                "settled indexing {case}"
            );
            if case == "paused" || case == "step" {
                app.world_mut()
                    .resource_mut::<Time>()
                    .advance_by(std::time::Duration::from_secs(1));
                app.update();
                app.update();
                assert_eq!(modified_events(&app, &mut cursor), 1, "real change {case}");
            }
        }
    }

    #[test]
    fn static_population_is_not_retained_or_revisited_on_settled_frames() {
        let mut app = observed_app();
        let handle = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial::default());
        for _ in 0..128 {
            app.world_mut().spawn(MeshMaterial3d(handle.clone()));
        }
        app.update();
        let index = app.world().resource::<MaterialUsers>();
        assert!(index.entities.is_empty() && index.materials.is_empty());
        let work = index.entity_visits;
        assert!(work >= 128, "counter must observe actual lifecycle visits");
        for _ in 0..4 {
            app.update();
        }
        assert_eq!(app.world().resource::<MaterialUsers>().entity_visits, work);
    }

    #[test]
    fn registration_replaces_payload_and_base_and_withdraws_a_missing_payload() {
        let mut assets = Assets::<StandardMaterial>::default();
        let handle = assets.add(StandardMaterial::default());
        let registry = MaterialAnimationRegistry::default();
        let mut index = AnimatedStandardMaterials::default();
        registry.insert(
            "fixture".into(),
            MaterialAnimation::from_extensions(&flame(), bevy::math::Affine2::IDENTITY).unwrap(),
        );
        index.register(
            handle.id(),
            Some("fixture"),
            assets.get(&handle),
            Some(&registry),
        );
        let base = bevy::math::Affine2::from_scale_angle_translation(
            Vec2::splat(3.0),
            0.0,
            Vec2::new(0.75, 0.2),
        );
        assets.get_mut(&handle).unwrap().uv_transform = base;
        let payload = serde_json::json!({"channels": [{"variable": "uOffset", "times": [0.0], "values": [0.1]}]});
        registry.insert(
            "fixture".into(),
            MaterialAnimation::from_extensions(&payload, bevy::math::Affine2::IDENTITY).unwrap(),
        );
        index.register(
            handle.id(),
            Some("fixture"),
            assets.get(&handle),
            Some(&registry),
        );
        assert_eq!(
            index.animated[&handle.id()].channels[0].variable,
            Variable::UOffset
        );
        assert_eq!(index.animated[&handle.id()].base_scale, Vec2::splat(3.0));
        assert_eq!(
            index.animated[&handle.id()].base_offset,
            Vec2::new(0.75, 0.2)
        );
        registry.replace("fixture", None);
        index.register(
            handle.id(),
            Some("fixture"),
            assets.get(&handle),
            Some(&registry),
        );
        assert!(!index.animated.contains_key(&handle.id()));
    }

    #[test]
    fn own_modified_does_not_rebind_but_an_external_modified_in_the_same_batch_does() {
        for external in [false, true] {
            let mut app = observed_app();
            let handle = app
                .world_mut()
                .resource_mut::<Assets<StandardMaterial>>()
                .add(StandardMaterial::default());
            app.update();
            register_fixture(&mut app, &handle, &flame());
            app.world_mut()
                .spawn((MeshMaterial3d(handle.clone()), ViewVisibility::VISIBLE));
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs(1));
            app.update(); // Playback queues one Modified, not yet flushed.
            if external {
                app.world_mut()
                    .resource_mut::<Assets<StandardMaterial>>()
                    .get_mut(&handle)
                    .unwrap()
                    .perceptual_roughness = 0.7;
            }
            let work = app.world().resource::<MaterialUsers>().entity_visits;
            app.update();
            let resolved = app.world().resource::<AnimatedStandardMaterials>();
            // A pathless fixture cannot re-resolve: an external event must withdraw it.
            assert_eq!(resolved.animated.contains_key(&handle.id()), !external);
            assert!(resolved.own_modified.is_empty());
            if !external {
                assert_eq!(app.world().resource::<MaterialUsers>().entity_visits, work);
            } else {
                assert!(app.world().resource::<MaterialUsers>().entities.is_empty());
            }
        }
    }

    #[test]
    fn loaded_with_dependencies_retries_and_withdraws_stale_resolution() {
        let mut app = observed_app();
        let handle = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial::default());
        app.update();
        register_fixture(&mut app, &handle, &flame());
        app.world_mut()
            .spawn((MeshMaterial3d(handle.clone()), ViewVisibility::VISIBLE));
        app.update();
        assert!(
            app.world()
                .resource::<MaterialUsers>()
                .materials
                .contains_key(&handle.id())
        );
        app.world_mut()
            .write_message(AssetEvent::<StandardMaterial>::LoadedWithDependencies {
                id: handle.id(),
            });
        app.update();
        assert!(
            !app.world()
                .resource::<AnimatedStandardMaterials>()
                .animated
                .contains_key(&handle.id())
        );
        assert!(app.world().resource::<MaterialUsers>().entities.is_empty());
    }

    #[test]
    fn insertion_swap_transfers_animated_ownership_without_an_asset_event_scan() {
        let mut app = observed_app();
        let first = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial::default());
        let second = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial::default());
        app.update();
        register_fixture(&mut app, &first, &flame());
        register_fixture(
            &mut app,
            &second,
            &serde_json::json!({"channels": [{
                "variable": "uOffset", "times": [0.0], "values": [0.75]
            }]}),
        );
        let entity = app
            .world_mut()
            .spawn((MeshMaterial3d(first.clone()), ViewVisibility::VISIBLE))
            .id();
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs(1));
        app.update();
        let played = app
            .world()
            .resource::<Assets<StandardMaterial>>()
            .get(&first)
            .unwrap()
            .uv_transform;
        let work = app.world().resource::<MaterialUsers>().entity_visits;
        app.world_mut()
            .entity_mut(entity)
            .insert(MeshMaterial3d(second.clone()));
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs(1));
        app.update();
        let index = app.world().resource::<MaterialUsers>();
        assert_eq!(index.entities[&entity], second.id());
        assert!(!index.materials.contains_key(&first.id()));
        assert_eq!(
            index.entity_visits,
            work + 2,
            "one discard and one insert, no scan"
        );
        let assets = app.world().resource::<Assets<StandardMaterial>>();
        assert_eq!(assets.get(&first).unwrap().uv_transform, played);
        assert_eq!(
            assets.get(&second).unwrap().uv_transform.translation,
            Vec2::new(0.75, 0.0)
        );
    }
}
