//! Session enable state shared by streamed roots and distant reference consumers.
//!
//! Events queued before the first catalog bind use the last value for each reference.
//! Reversible visibility applies to indexed live roots when cluster mode installs this service.
//! Re-admission of missing bodies is limited to cluster-owned exterior refused static scenery; enabling
//! an absent interior, non-cluster or fully owned body does not spawn it through this API.
//! The first writer invocation warns about this scope once per session, without frame scans.

use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    sync::Arc,
};

use crate::cluster::flat_source::{FlatSourceOwner, FlatSourcePrimitives};
use bevy::prelude::*;
use color_eyre::{Result, eyre::WrapErr};
use rusqlite::Connection;

use super::{
    components::FormId,
    enable_state::{
        EnableState, EnableStateError, EnableStateResolver, InitialEnableInputs, PLAYER_REFERENCE,
    },
};

/// Immutable enable definitions and initial resolution for one winning-record database.
pub struct EnableCatalog {
    definitions: HashMap<u32, InitialEnableInputs>,
    children: HashMap<u32, Vec<u32>>,
    initial: EnableStateResolver,
}

impl EnableCatalog {
    /// Reads the winning reference table once; runtime resolution performs no database IO.
    pub fn read(connection: &Connection) -> Result<Self> {
        let count: usize = connection.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('references') \
             WHERE name IN ('header_flags','enable_parent_id','enable_parent_flags')",
            [],
            |row| row.get(0),
        )?;
        let columns = match count {
            0 => {
                warn!(
                    "world database has no reference enable-state metadata; references default to enabled; reconvert assets to filter initially disabled references"
                );
                "0,NULL,0"
            }
            3 => "header_flags,enable_parent_id,enable_parent_flags",
            _ => color_eyre::eyre::bail!(
                "incomplete reference enable-state columns: expected header_flags, enable_parent_id and enable_parent_flags"
            ),
        };
        let mut statement = connection.prepare(&format!(
            "SELECT id,{columns} FROM \"references\" ORDER BY id"
        ))?;
        let mut definitions = HashMap::new();
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut initial = EnableStateResolver::default();
        let mut rows = statement.query([])?;
        let mut order = Vec::new();
        while let Some(row) = rows.next()? {
            let reference = row.get(0)?;
            let inputs = super::database::map_enable_inputs(row, 1)
                .wrap_err_with(|| format!("enable metadata of reference {reference:08X}"))?;
            // The resolver's PlayerRef bootstrap is a terminal seed even if a row exists.
            if reference != PLAYER_REFERENCE
                && let Some(parent) = inputs.parent_id.filter(|parent| *parent != 0)
            {
                children.entry(parent).or_default().push(reference);
            }
            definitions.insert(reference, inputs);
            order.push(reference);
        }
        let mut diagnosed = HashSet::new();
        for reference in order {
            let state = resolve_definition(&mut initial, &definitions, reference);
            if let Err(error) = state
                && diagnosed.insert(error)
            {
                warn!(reference, %error, "unresolved reference enable state stays hidden");
            }
        }
        Ok(Self {
            definitions,
            children,
            initial,
        })
    }

    /// Initial state, unaffected by session overrides; unresolved or absent IDs return `None`.
    pub fn enabled(&self, reference: u32) -> Option<bool> {
        self.initial.cached(reference).and_then(Result::ok)
    }
}

fn resolve_definition(
    resolver: &mut EnableStateResolver,
    definitions: &HashMap<u32, InitialEnableInputs>,
    reference: u32,
) -> EnableState {
    if let Some(state) = resolver.cached(reference) {
        return state;
    }
    // Only known references are invalidated; PlayerRef always receives an explicit seed.
    let Some(inputs) = definitions.get(&reference).copied() else {
        return Err(EnableStateError::MissingParent(reference));
    };
    resolver
        .resolve(reference, inputs, |parent| {
            Ok::<_, Infallible>(definitions.get(&parent).copied())
        })
        .unwrap_or_else(|never| match never {})
}

/// Temporary reference gate, separate from each source node's authored visibility.
/// Also mark visibility-less pivots: moving a gated subtree then needs no body scan
/// to decide whether its old gate must be removed. Despawn cleans up the marker.
#[derive(Component, Clone, Copy)]
#[component(storage = "SparseSet")]
pub(crate) struct ReferenceVisibilityGate(pub(crate) Option<Visibility>);

struct RegisteredRoot {
    reference: u32,
    saved_visibility: Option<Visibility>,
}

/// Runtime state survives entity unload/reload and repeated binding of the same catalog.
#[derive(Resource, Default)]
pub struct ReferenceEnableState {
    catalog: Option<Arc<EnableCatalog>>,
    resolver: EnableStateResolver,
    overrides: HashMap<u32, bool>,
    bind_changes: Vec<ReferenceEnableChanged>,
    scope_warned: bool,
    revision: u64,
    roots: HashMap<u32, HashSet<Entity>>,
    entities: HashMap<Entity, RegisteredRoot>,
    #[cfg(test)]
    recomputed: Vec<u32>,
}

impl ReferenceEnableState {
    /// The first bind applies queued overrides; later changed winning inputs reset them.
    /// Recertifying the same inputs preserves session state. The caller reconciles surviving
    /// roots after a true result.
    pub fn bind(&mut self, catalog: Arc<EnableCatalog>) -> bool {
        if self.catalog.as_ref().is_some_and(|current| {
            Arc::ptr_eq(current, &catalog) || current.definitions == catalog.definitions
        }) {
            self.catalog = Some(catalog);
            return false;
        }
        self.bind_changes.clear();
        self.resolver = catalog.initial.clone();
        if self.catalog.is_none() {
            self.overrides.retain(|reference, _| {
                let known =
                    *reference == PLAYER_REFERENCE || catalog.definitions.contains_key(reference);
                if !known {
                    warn!(reference, "cannot set enable state of an unknown reference");
                }
                known
            });
            // Invalidate the affected cache before seeding every explicit override together.
            // This preserves child insulation regardless of queued event or map iteration order.
            let mut pending: Vec<_> = self.overrides.keys().copied().collect();
            let mut affected = HashSet::new();
            while let Some(reference) = pending.pop() {
                if affected.insert(reference) {
                    self.resolver.invalidate(reference);
                    if let Some(children) = catalog.children.get(&reference) {
                        pending.extend(children.iter().copied());
                    }
                }
            }
            for (&reference, &enabled) in &self.overrides {
                self.resolver.seed(reference, Ok(enabled));
            }
            for reference in affected {
                if let Ok(enabled) =
                    resolve_definition(&mut self.resolver, &catalog.definitions, reference)
                    && catalog.enabled(reference) != Some(enabled)
                {
                    self.bind_changes
                        .push(ReferenceEnableChanged { reference, enabled });
                }
            }
        } else {
            self.overrides.clear();
        }
        self.catalog = Some(catalog);
        self.revision = self.revision.wrapping_add(1);
        #[cfg(test)]
        self.recomputed.clear();
        true
    }

    /// Reconcile the existing identity index once when winning inputs change, never per frame.
    fn reconcile_roots(&mut self, world: &mut World) {
        let entities: Vec<_> = self.entities.keys().copied().collect();
        for &entity in &entities {
            if world.get::<FormId>(entity).map(|form| form.0)
                != self.entities.get(&entity).map(|root| root.reference)
            {
                continue;
            }
            let authored = world
                .get::<ReferenceVisibilityGate>(entity)
                .and_then(|gate| gate.0);
            if let Some(mut visibility) = world.get_mut::<Visibility>(entity) {
                self.apply_visibility(entity, &mut visibility, authored);
            }
        }
        for entity in entities {
            reconcile_source_subtree(world, self, entity);
        }
    }

    pub fn enabled(&self, reference: u32) -> Option<bool> {
        self.catalog.as_ref()?;
        self.resolver.cached(reference).and_then(Result::ok)
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn is_bound(&self) -> bool {
        self.catalog.is_some()
    }

    fn set(&mut self, reference: u32, enabled: bool) -> Vec<ReferenceEnableChanged> {
        if !self.scope_warned {
            self.scope_warned = true;
            warn!(
                "reference enable API scope requires a certified cluster catalog: reversible visibility applies to indexed live roots; missing-body re-admission is limited to cluster-owned exterior refused static scenery; pre-bind requests are queued"
            );
        }
        let Some(catalog) = self.catalog.as_ref() else {
            self.overrides.insert(reference, enabled);
            return Vec::new();
        };
        if reference != PLAYER_REFERENCE && !catalog.definitions.contains_key(&reference) {
            warn!(reference, "cannot set enable state of an unknown reference");
            return Vec::new();
        }
        if self.overrides.get(&reference) == Some(&enabled) {
            return Vec::new();
        }
        self.overrides.insert(reference, enabled);
        // Even an unchanged effective value creates a persistent explicit override.
        if self.resolver.cached(reference) == Some(Ok(enabled)) {
            return Vec::new();
        }
        let mut affected = Vec::new();
        let mut pending = vec![reference];
        let mut visited = HashSet::new();
        while let Some(id) = pending.pop() {
            if !visited.insert(id) {
                continue;
            }
            // An explicitly overridden child insulates its whole subtree from this change.
            if id != reference && self.overrides.contains_key(&id) {
                continue;
            }
            affected.push((id, self.resolver.cached(id).and_then(Result::ok)));
            if let Some(children) = catalog.children.get(&id) {
                pending.extend(children.iter().copied());
            }
        }
        for (id, _) in &affected {
            self.resolver.invalidate(*id);
        }
        self.resolver.seed(reference, Ok(enabled));
        let mut changes = Vec::new();
        for (id, before) in affected {
            #[cfg(test)]
            self.recomputed.push(id);
            let state = resolve_definition(&mut self.resolver, &catalog.definitions, id);
            if let Ok(enabled) = state
                && before != Some(enabled)
            {
                changes.push(ReferenceEnableChanged {
                    reference: id,
                    enabled,
                });
            }
        }
        if !changes.is_empty() {
            self.revision = self.revision.wrapping_add(1);
        }
        changes
    }

    fn register(
        &mut self,
        entity: Entity,
        reference: u32,
        visibility: Option<&mut Visibility>,
        authored: Option<Visibility>,
    ) {
        if self
            .entities
            .get(&entity)
            .is_some_and(|root| root.reference == reference)
        {
            if let Some(visibility) = visibility {
                self.apply_visibility(entity, visibility, authored);
            }
            return;
        }
        let mut visibility = visibility;
        self.unregister(entity, visibility.as_deref_mut());
        self.entities.insert(
            entity,
            RegisteredRoot {
                reference,
                saved_visibility: None,
            },
        );
        self.roots.entry(reference).or_default().insert(entity);
        if let Some(visibility) = visibility {
            self.apply_visibility(entity, visibility, authored);
        }
    }

    fn unregister(&mut self, entity: Entity, visibility: Option<&mut Visibility>) {
        let Some(root) = self.entities.remove(&entity) else {
            return;
        };
        if let Some(roots) = self.roots.get_mut(&root.reference) {
            roots.remove(&entity);
            if roots.is_empty() {
                self.roots.remove(&root.reference);
            }
        }
        if let Some(visibility) = visibility
            && let Some(saved) = root.saved_visibility
            && *visibility != saved
        {
            *visibility = saved;
        }
    }

    fn apply_visibility(
        &mut self,
        entity: Entity,
        visibility: &mut Visibility,
        authored: Option<Visibility>,
    ) {
        if !self.is_bound() {
            return;
        }
        let Some(reference) = self.entities.get(&entity).map(|root| root.reference) else {
            return;
        };
        let enabled = self.enabled(reference) == Some(true);
        let Some(root) = self.entities.get_mut(&entity) else {
            return;
        };
        let desired = if enabled {
            let saved = root.saved_visibility.take();
            authored.or(saved).unwrap_or(*visibility)
        } else {
            // A nested reference may already be hidden by an ancestor's gate.
            root.saved_visibility
                .get_or_insert(authored.unwrap_or(*visibility));
            Visibility::Hidden
        };
        if *visibility != desired {
            *visibility = desired;
        }
    }
}

/// Publish bind changes only after reconciliation restores the state resource. Component
/// insertion may flush world commands while `resource_scope` has temporarily removed it.
pub(crate) fn reconcile_bound_roots(world: &mut World) {
    let changes = world.resource_scope(|world, mut state: Mut<ReferenceEnableState>| {
        state.reconcile_roots(world);
        std::mem::take(&mut state.bind_changes)
    });
    for change in changes {
        world.trigger(change);
    }
}

/// Explicitly enable or disable a placed reference for this session.
///
/// Before the first catalog bind, the last request per reference is queued. Equal-input
/// recertification preserves overrides; changed winning inputs reset a bound session.
/// Indexed live roots support reversible visibility. Missing-body re-admission is limited
/// to cluster-owned exterior refused static scenery: this event does not provide a general
/// spawn path for absent interiors, non-cluster references or fully owned geometry.
/// The first invocation warns once per session about this scope.
#[derive(Event, Clone, Copy, Debug)]
pub struct SetReferenceEnabled {
    pub reference: u32,
    pub enabled: bool,
}

/// Effective enable-state change, separate from permanent pose or gameplay revocations.
#[derive(Event, Clone, Copy, Debug)]
pub struct ReferenceEnableChanged {
    pub reference: u32,
    pub enabled: bool,
}

pub fn install(app: &mut App) {
    if app.world().contains_resource::<EnableObserversInstalled>() {
        return;
    }
    app.init_resource::<ReferenceEnableState>()
        .init_resource::<EnableObserversInstalled>()
        .add_observer(set_reference_enabled)
        .add_observer(register_form_id)
        .add_observer(remove_form_id)
        .add_observer(register_visibility)
        .add_observer(discard_visibility)
        .add_observer(source_graph_inserted)
        .add_observer(source_graph_removed);
}

#[derive(Resource, Default)]
struct EnableObserversInstalled;

fn set_reference_enabled(
    event: On<SetReferenceEnabled>,
    mut world: bevy::ecs::world::DeferredWorld,
) {
    let changes = world
        .resource_mut::<ReferenceEnableState>()
        .set(event.reference, event.enabled);
    for change in changes {
        let entities: Vec<_> = world
            .resource::<ReferenceEnableState>()
            .roots
            .get(&change.reference)
            .map(|roots| roots.iter().copied().collect())
            .unwrap_or_default();
        for entity in entities {
            if world
                .get::<FormId>(entity)
                .is_some_and(|id| id.0 == change.reference)
                && let Some(mut desired) = world.get::<Visibility>(entity).copied()
            {
                let authored = world
                    .get::<ReferenceVisibilityGate>(entity)
                    .and_then(|gate| gate.0);
                world
                    .resource_mut::<ReferenceEnableState>()
                    .apply_visibility(entity, &mut desired, authored);
                if let Some(mut visibility) = world.get_mut::<Visibility>(entity)
                    && *visibility != desired
                {
                    *visibility = desired;
                }
            }
            queue_source_subtree(&mut world.commands(), entity);
        }
        world.commands().trigger(change);
    }
}

fn register_reference(world: &mut bevy::ecs::world::DeferredWorld, entity: Entity) {
    if let Some(reference) = world.get::<FormId>(entity).map(|id| id.0) {
        let authored = world
            .get::<ReferenceVisibilityGate>(entity)
            .and_then(|gate| gate.0);
        let mut desired = world.get::<Visibility>(entity).copied();
        world.resource_mut::<ReferenceEnableState>().register(
            entity,
            reference,
            desired.as_mut(),
            authored,
        );
        if let Some(desired) = desired
            && let Some(mut visibility) = world.get_mut::<Visibility>(entity)
            && *visibility != desired
        {
            *visibility = desired;
        }
    }
    queue_source_subtree(&mut world.commands(), entity);
}

fn register_form_id(event: On<Insert, FormId>, mut world: bevy::ecs::world::DeferredWorld) {
    register_reference(&mut world, event.entity);
}

fn remove_form_id(event: On<Remove, FormId>, mut world: bevy::ecs::world::DeferredWorld) {
    let mut desired = world.get::<Visibility>(event.entity).copied();
    world
        .resource_mut::<ReferenceEnableState>()
        .unregister(event.entity, desired.as_mut());
    if let Some(desired) = desired
        && let Some(mut visibility) = world.get_mut::<Visibility>(event.entity)
        && *visibility != desired
    {
        *visibility = desired;
    }
    queue_source_subtree(&mut world.commands(), event.entity);
}

fn register_visibility(event: On<Insert, Visibility>, mut world: bevy::ecs::world::DeferredWorld) {
    register_reference(&mut world, event.entity);
}

fn discard_visibility(event: On<Discard, Visibility>, mut world: bevy::ecs::world::DeferredWorld) {
    if let Some(root) = world
        .resource_mut::<ReferenceEnableState>()
        .entities
        .get_mut(&event.entity)
    {
        root.saved_visibility = None;
    }
    // A replacement is a new authored value; removing Visibility must not keep
    // a stale value that could overwrite a later insertion.
    if let Some(mut gate) = world.get_mut::<ReferenceVisibilityGate>(event.entity) {
        gate.0 = None;
    }
}

fn source_graph_inserted(
    event: On<Insert, (Mesh3d, ChildOf, FlatSourceOwner)>,
    mut commands: Commands,
) {
    queue_source_subtree(&mut commands, event.entity);
}

fn source_graph_removed(event: On<Remove, (ChildOf, FlatSourceOwner)>, mut commands: Commands) {
    queue_source_subtree(&mut commands, event.entity);
}

fn queue_source_subtree(commands: &mut Commands, entity: Entity) {
    // Relationship backlinks and removals must finish before walking the graph.
    commands.queue(move |world: &mut World| {
        world.resource_scope(|world, state: Mut<ReferenceEnableState>| {
            reconcile_source_subtree(world, &state, entity);
        });
    });
}

fn source_parent(world: &World, entity: Entity) -> Option<Entity> {
    world
        .get::<ChildOf>(entity)
        .map(ChildOf::parent)
        .or_else(|| world.get::<FlatSourceOwner>(entity).map(|owner| owner.0))
}

fn reference_is_disabled(world: &World, state: &ReferenceEnableState, entity: Entity) -> bool {
    state.is_bound()
        && state.entities.get(&entity).is_some_and(|root| {
            world.get::<FormId>(entity).map(|form| form.0) == Some(root.reference)
                && state.enabled(root.reference) != Some(true)
        })
}

fn source_has_disabled_ancestor(
    world: &World,
    state: &ReferenceEnableState,
    entity: Entity,
) -> bool {
    let mut current = Some(entity);
    let mut visited = HashSet::new();
    while let Some(entity) = current {
        if !visited.insert(entity) || reference_is_disabled(world, state, entity) {
            return true;
        }
        current = source_parent(world, entity);
    }
    false
}

fn source_parent_visible(world: &World, entity: Entity) -> bool {
    let mut current = source_parent(world, entity);
    let mut visited = HashSet::new();
    while let Some(entity) = current {
        if !visited.insert(entity) {
            return false;
        }
        // Match Bevy's missing-parent-component rule, but evaluate the authored
        // chain directly: a Disabled parent's cached inherited value can be stale.
        if world.get::<InheritedVisibility>(entity).is_none() {
            return true;
        }
        match world.get::<Visibility>(entity) {
            Some(Visibility::Hidden) => return false,
            Some(Visibility::Visible) | None => return true,
            Some(Visibility::Inherited) => current = source_parent(world, entity),
        }
    }
    true
}

fn reconcile_source_subtree(world: &mut World, state: &ReferenceEnableState, root: Entity) {
    if world.get_entity(root).is_err() {
        return;
    }
    let disabled = source_has_disabled_ancestor(world, state, root);
    // Ordinary enabled scene insertion costs an ancestry walk, never a subtree
    // scan. Every gated pivot carries a marker, so reparenting can undo its gate.
    if !disabled && world.get::<ReferenceVisibilityGate>(root).is_none() {
        return;
    }
    let mut pending = vec![(root, disabled, source_parent_visible(world, root))];
    let mut visited = HashSet::new();
    while let Some((entity, ancestor_disabled, parent_visible)) = pending.pop() {
        if !visited.insert(entity) || world.get_entity(entity).is_err() {
            continue;
        }
        let disabled = ancestor_disabled || reference_is_disabled(world, state, entity);
        let gate = world.get::<ReferenceVisibilityGate>(entity).copied();
        let current = world.get::<Visibility>(entity).copied();
        let visibility = if disabled {
            let authored = gate
                .and_then(|gate| gate.0)
                .or_else(|| {
                    state
                        .entities
                        .get(&entity)
                        .and_then(|root| root.saved_visibility)
                })
                .or(current);
            if gate.is_none_or(|gate| gate.0 != authored) {
                world
                    .entity_mut(entity)
                    .insert(ReferenceVisibilityGate(authored));
            }
            current.map(|_| Visibility::Hidden)
        } else {
            if gate.is_some() {
                world.entity_mut(entity).remove::<ReferenceVisibilityGate>();
            }
            gate.and_then(|gate| gate.0).or(current)
        };
        // Mutate in place: Insert observers must not recursively schedule us.
        if let (Some(next), Some(mut current)) = (visibility, world.get_mut::<Visibility>(entity))
            && *current != next
        {
            *current = next;
        }
        let visible = !disabled
            && match visibility {
                Some(Visibility::Hidden) => false,
                Some(Visibility::Inherited) => parent_visible,
                Some(Visibility::Visible) | None => true,
            };
        if let Some(mut inherited) = world.get_mut::<InheritedVisibility>(entity) {
            let next = if visible {
                InheritedVisibility::VISIBLE
            } else {
                InheritedVisibility::HIDDEN
            };
            if *inherited != next {
                *inherited = next;
            }
        }
        let mut children: Vec<_> = world
            .get::<Children>(entity)
            .map(|children| children.iter().collect())
            .unwrap_or_default();
        if let Some(flat) = world.get::<FlatSourcePrimitives>(entity) {
            children.extend(flat.iter());
        }
        for child in children {
            // ChildOf takes precedence over flat ownership, as in source identity.
            if source_parent(world, child) == Some(entity) {
                pending.push((child, disabled, visible));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{components::FormId, enable_state::PLAYER_REFERENCE};
    use bevy::ecs::entity_disabling::Disabled;

    #[derive(Resource, Default)]
    struct Changed(Vec<(u32, bool)>);

    fn database(rows: &str) -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(&format!(
                "CREATE TABLE \"references\" (id INTEGER PRIMARY KEY, header_flags INTEGER NOT NULL, \
                 enable_parent_id INTEGER, enable_parent_flags INTEGER); \
                 INSERT INTO \"references\" VALUES {rows};"
            ))
            .unwrap();
        connection
    }

    fn catalog(rows: &str) -> Arc<EnableCatalog> {
        Arc::new(EnableCatalog::read(&database(rows)).unwrap())
    }

    fn app(catalog: Arc<EnableCatalog>) -> App {
        let mut app = App::new();
        install(&mut app);
        app.init_resource::<Changed>().add_observer(
            |event: On<ReferenceEnableChanged>, mut changed: ResMut<Changed>| {
                changed.0.push((event.reference, event.enabled));
            },
        );
        app.world_mut()
            .resource_mut::<ReferenceEnableState>()
            .bind(catalog);
        app
    }

    fn set(app: &mut App, reference: u32, enabled: bool) {
        app.world_mut()
            .trigger(SetReferenceEnabled { reference, enabled });
        app.world_mut().flush();
    }

    fn enabled(app: &App, reference: u32) -> Option<bool> {
        app.world()
            .resource::<ReferenceEnableState>()
            .enabled(reference)
    }

    fn revision(app: &App) -> u64 {
        app.world().resource::<ReferenceEnableState>().revision()
    }

    fn unbound_app() -> App {
        let mut app = App::new();
        install(&mut app);
        app.init_resource::<Changed>().add_observer(
            |event: On<ReferenceEnableChanged>, mut changed: ResMut<Changed>| {
                changed.0.push((event.reference, event.enabled));
            },
        );
        app
    }

    fn bind_and_reconcile(app: &mut App, catalog: Arc<EnableCatalog>) {
        if app
            .world_mut()
            .resource_mut::<ReferenceEnableState>()
            .bind(catalog)
        {
            reconcile_bound_roots(app.world_mut());
        }
        app.world_mut().flush();
    }

    #[test]
    fn pre_bind_event_applies_to_registered_root_at_initial_bind() {
        let mut app = unbound_app();
        let root = app.world_mut().spawn((FormId(1), Visibility::Visible)).id();
        set(&mut app, 1, false);
        assert_eq!(enabled(&app, 1), None);
        assert_eq!(
            app.world().get::<Visibility>(root),
            Some(&Visibility::Visible)
        );
        assert!(app.world().resource::<Changed>().0.is_empty());

        bind_and_reconcile(&mut app, catalog("(1,0,NULL,0)"));
        assert_eq!(enabled(&app, 1), Some(false));
        assert_eq!(
            app.world().get::<Visibility>(root),
            Some(&Visibility::Hidden)
        );
        set(&mut app, 1, true);
        assert_eq!(
            app.world().get::<Visibility>(root),
            Some(&Visibility::Visible)
        );
    }

    #[test]
    fn pre_bind_replay_observers_can_read_restored_state_while_gating_live_roots() {
        #[derive(Resource, Default)]
        struct Readback(Vec<(u32, Option<bool>)>);
        let mut app = unbound_app();
        app.init_resource::<Readback>().add_observer(
            |event: On<ReferenceEnableChanged>,
             state: Res<ReferenceEnableState>,
             mut seen: ResMut<Readback>| {
                seen.0
                    .push((event.reference, state.enabled(event.reference)));
            },
        );
        let root = app.world_mut().spawn((FormId(1), Visibility::Visible)).id();
        app.world_mut()
            .spawn((Mesh3d::default(), Visibility::Visible, ChildOf(root)));
        set(&mut app, 1, false);
        bind_and_reconcile(&mut app, catalog("(1,0,NULL,0)"));
        assert_eq!(app.world().resource::<Readback>().0, [(1, Some(false))]);
        assert_eq!(
            app.world().get::<Visibility>(root),
            Some(&Visibility::Hidden)
        );
    }

    #[test]
    fn pre_bind_repeated_events_use_last_value_per_reference() {
        let mut app = unbound_app();
        let before = revision(&app);
        for (reference, value) in [
            (1, false),
            (2, true),
            (1, true),
            (2, false),
            (1, false),
            (2, true),
        ] {
            set(&mut app, reference, value);
        }
        assert_eq!(revision(&app), before);
        assert!(app.world().resource::<Changed>().0.is_empty());
        bind_and_reconcile(&mut app, catalog("(1,0,NULL,0),(2,2048,NULL,0)"));
        assert_eq!(
            (enabled(&app, 1), enabled(&app, 2)),
            (Some(false), Some(true))
        );
        let root = app
            .world_mut()
            .spawn((FormId(1), Visibility::Inherited))
            .id();
        assert_eq!(
            app.world().get::<Visibility>(root),
            Some(&Visibility::Hidden)
        );
        let root = app.world_mut().spawn((FormId(2), Visibility::Visible)).id();
        assert_eq!(
            app.world().get::<Visibility>(root),
            Some(&Visibility::Visible)
        );
    }

    #[test]
    fn pre_bind_explicit_child_insulates_descendants_in_either_event_order() {
        for events in [[(1, false), (2, true)], [(2, true), (1, false)]] {
            let mut app = unbound_app();
            for (reference, value) in events {
                set(&mut app, reference, value);
            }
            bind_and_reconcile(&mut app, catalog("(1,0,NULL,0),(2,0,1,0),(3,0,2,1)"));
            assert_eq!(
                (enabled(&app, 1), enabled(&app, 2), enabled(&app, 3)),
                (Some(false), Some(true), Some(false)),
                "queued events: {events:?}",
            );
            set(&mut app, 1, true);
            app.world_mut().resource_mut::<Changed>().0.clear();
            set(&mut app, 1, false);
            assert_eq!(
                (enabled(&app, 2), enabled(&app, 3)),
                (Some(true), Some(false))
            );
            assert_eq!(app.world().resource::<Changed>().0, [(1, false)]);
        }
    }

    #[test]
    fn pre_bind_override_survives_equal_recertification_but_not_changed_inputs() {
        let mut app = unbound_app();
        set(&mut app, 1, false);
        bind_and_reconcile(&mut app, catalog("(1,0,NULL,0)"));
        assert_eq!(enabled(&app, 1), Some(false));
        let before = revision(&app);
        bind_and_reconcile(&mut app, catalog("(1,0,NULL,0)"));
        assert_eq!(enabled(&app, 1), Some(false));
        assert_eq!(revision(&app), before);
        bind_and_reconcile(&mut app, catalog("(1,0,NULL,0),(2,0,NULL,0)"));
        assert_eq!(enabled(&app, 1), Some(true));
        assert!(
            app.world()
                .resource::<ReferenceEnableState>()
                .overrides
                .is_empty()
        );
    }

    #[test]
    fn pre_bind_unknown_reference_is_rejected_but_player_override_is_kept() {
        let mut app = unbound_app();
        set(&mut app, 99, false);
        set(&mut app, PLAYER_REFERENCE, false);
        bind_and_reconcile(&mut app, catalog("(1,0,20,0)"));
        assert_eq!(enabled(&app, PLAYER_REFERENCE), Some(false));
        assert_eq!(enabled(&app, 1), Some(false));
        assert_eq!(enabled(&app, 99), None);
        assert!(
            !app.world()
                .resource::<ReferenceEnableState>()
                .overrides
                .contains_key(&99)
        );
    }

    fn assert_gate(app: &App, draw: Entity, hidden: bool) {
        assert_eq!(
            app.world().get::<Visibility>(draw),
            Some(&if hidden {
                Visibility::Hidden
            } else {
                Visibility::Visible
            }),
        );
        assert_eq!(
            app.world().get::<ReferenceVisibilityGate>(draw).is_some(),
            hidden
        );
        assert_eq!(
            app.world().get::<InheritedVisibility>(draw).unwrap().get(),
            !hidden
        );
        assert!(app.world().resource::<Changed>().0.is_empty());
    }

    #[test]
    fn tuple_observers_reconcile_flat_owner_insert_reparent_and_remove_without_enable_events() {
        // Bevy 0.19 On<E, (A, B)> matches either component, not a query requiring both.
        // Keep this harness free of scheduled visibility systems: graph observers must suffice.
        let mut app = app(catalog("(1,2048,NULL,0),(2,0,NULL,0)"));
        let disabled = app
            .world_mut()
            .spawn((FormId(1), Visibility::Inherited))
            .id();
        let enabled = app
            .world_mut()
            .spawn((FormId(2), Visibility::Inherited))
            .id();
        let draw = app
            .world_mut()
            .spawn((Mesh3d::default(), Visibility::Visible))
            .id();
        app.world_mut()
            .entity_mut(draw)
            .insert(FlatSourceOwner(disabled));
        app.world_mut().flush();
        assert!(app.world().get::<ChildOf>(draw).is_none());
        assert_gate(&app, draw, true);

        app.world_mut()
            .entity_mut(draw)
            .insert(FlatSourceOwner(enabled));
        app.world_mut().flush();
        assert_gate(&app, draw, false);
        app.world_mut()
            .entity_mut(draw)
            .insert(FlatSourceOwner(disabled));
        app.world_mut().flush();
        assert_gate(&app, draw, true);
        app.world_mut().entity_mut(draw).remove::<FlatSourceOwner>();
        app.world_mut().flush();
        assert_gate(&app, draw, false);
    }

    #[test]
    fn tuple_observers_reconcile_child_of_edits_without_flat_owner_or_enable_events() {
        let mut app = app(catalog("(1,2048,NULL,0),(2,0,NULL,0)"));
        let disabled = app
            .world_mut()
            .spawn((FormId(1), Visibility::Inherited))
            .id();
        let enabled = app
            .world_mut()
            .spawn((FormId(2), Visibility::Inherited))
            .id();
        let draw = app
            .world_mut()
            .spawn((Mesh3d::default(), Visibility::Visible))
            .id();
        app.world_mut().entity_mut(draw).insert(ChildOf(disabled));
        app.world_mut().flush();
        assert!(app.world().get::<FlatSourceOwner>(draw).is_none());
        assert_gate(&app, draw, true);

        app.world_mut().entity_mut(draw).insert(ChildOf(enabled));
        app.world_mut().flush();
        assert_gate(&app, draw, false);
        app.world_mut().entity_mut(draw).insert(ChildOf(disabled));
        app.world_mut().flush();
        assert_gate(&app, draw, true);
        app.world_mut().entity_mut(draw).remove::<ChildOf>();
        app.world_mut().flush();
        assert_gate(&app, draw, false);
    }

    #[derive(Clone, Default)]
    struct ScopeWarnings(Arc<std::sync::Mutex<Vec<String>>>);

    impl tracing::Subscriber for ScopeWarnings {
        fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
            *metadata.level() == tracing::Level::WARN
        }

        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            struct Message(String);
            impl tracing::field::Visit for Message {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    if field.name() == "message" {
                        self.0 = format!("{value:?}");
                    }
                }
            }
            let mut message = Message(String::new());
            event.record(&mut message);
            if message.0.contains("reference enable API scope") {
                self.0.lock().unwrap().push(message.0);
            }
        }
    }

    #[test]
    fn api_scope_warns_once_per_session_including_pre_bind_calls_and_recertification() {
        let warnings = ScopeWarnings::default();
        tracing::subscriber::with_default(warnings.clone(), || {
            for session in 1..=2 {
                let mut app = unbound_app();
                set(&mut app, 1, false);
                set(&mut app, 1, false);
                bind_and_reconcile(&mut app, catalog("(1,0,NULL,0)"));
                set(&mut app, 1, true);
                bind_and_reconcile(&mut app, catalog("(1,0,NULL,0),(2,0,NULL,0)"));
                set(&mut app, 2, false);
                install(&mut app);
                set(&mut app, 2, true);
                assert_eq!(warnings.0.lock().unwrap().len(), session);
            }
        });
        for warning in warnings.0.lock().unwrap().iter() {
            assert!(warning.contains("indexed live roots"), "{warning}");
            assert!(warning.contains("cluster-owned exterior"), "{warning}");
            assert!(warning.contains("refused static scenery"), "{warning}");
        }
    }

    #[test]
    fn install_is_idempotent_and_unchanged_events_leave_visibility_untouched() {
        let mut app = app(catalog("(1,0,NULL,0)"));
        let observer_count = app
            .world_mut()
            .query::<&Observer>()
            .iter(app.world())
            .count();
        install(&mut app);
        assert_eq!(
            app.world_mut()
                .query::<&Observer>()
                .iter(app.world())
                .count(),
            observer_count
        );
        let root = app.world_mut().spawn((FormId(1), Visibility::Visible)).id();
        app.world_mut().clear_trackers();
        let before = revision(&app);
        set(&mut app, 1, true);
        assert_eq!(revision(&app), before);
        assert!(
            !app.world()
                .entity(root)
                .get_ref::<Visibility>()
                .unwrap()
                .is_changed()
        );
        assert!(app.world().resource::<Changed>().0.is_empty());
    }

    #[test]
    fn initially_disabled_reference_stays_hidden_until_runtime_enable() {
        let mut app = app(catalog("(1,2048,NULL,NULL)"));
        assert!(app.world().resource::<ReferenceEnableState>().is_bound());
        assert_eq!(enabled(&app, 1), Some(false));
        let root = app
            .world_mut()
            .spawn((FormId(1), Visibility::Inherited))
            .id();
        assert_eq!(
            app.world().get::<Visibility>(root),
            Some(&Visibility::Hidden)
        );
        let before = revision(&app);
        set(&mut app, 1, true);
        assert_eq!(enabled(&app, 1), Some(true));
        assert_eq!(
            app.world().get::<Visibility>(root),
            Some(&Visibility::Inherited)
        );
        assert_eq!(revision(&app), before + 1);
        assert_eq!(app.world().resource::<Changed>().0, [(1, true)]);
    }

    #[test]
    fn parent_toggle_updates_only_transitive_dependents_and_opposite_chains() {
        let mut app = app(catalog(
            "(1,0,NULL,0),(2,2048,1,0),(3,0,2,1),(4,0,3,1),(9,0,NULL,0)",
        ));
        set(&mut app, 1, false);
        assert_eq!(
            (
                enabled(&app, 1),
                enabled(&app, 2),
                enabled(&app, 3),
                enabled(&app, 4)
            ),
            (Some(false), Some(false), Some(true), Some(false))
        );
        assert_eq!(enabled(&app, 9), Some(true));
        let mut changed = app.world().resource::<Changed>().0.clone();
        changed.sort_unstable();
        assert_eq!(changed, [(1, false), (2, false), (3, true), (4, false)]);
        let state = app.world().resource::<ReferenceEnableState>();
        let mut recomputed = state.recomputed.clone();
        recomputed.sort_unstable();
        assert_eq!(recomputed, [1, 2, 3, 4]);
        set(&mut app, 1, true);
        assert_eq!(
            (enabled(&app, 2), enabled(&app, 3), enabled(&app, 4)),
            (Some(true), Some(false), Some(true))
        );
    }

    #[test]
    fn explicit_override_survives_parent_changes_even_when_originally_unchanged() {
        let mut app = app(catalog("(1,0,NULL,0),(2,0,1,0),(3,0,2,1)"));
        let before = revision(&app);
        set(&mut app, 2, true);
        assert_eq!(revision(&app), before);
        assert!(app.world().resource::<Changed>().0.is_empty());
        assert!(
            app.world()
                .resource::<ReferenceEnableState>()
                .recomputed
                .is_empty()
        );
        set(&mut app, 1, false);
        assert_eq!(enabled(&app, 1), Some(false));
        assert_eq!(enabled(&app, 2), Some(true));
        assert_eq!(enabled(&app, 3), Some(false));
        assert_eq!(app.world().resource::<Changed>().0, [(1, false)]);
    }

    #[test]
    fn repeated_event_has_no_revision_notifications_or_resolution_work() {
        let mut app = app(catalog("(1,0,NULL,0),(2,0,1,0)"));
        set(&mut app, 1, false);
        let before = revision(&app);
        let work = app
            .world()
            .resource::<ReferenceEnableState>()
            .recomputed
            .clone();
        let notifications = app.world().resource::<Changed>().0.clone();
        set(&mut app, 1, false);
        assert_eq!(enabled(&app, 2), Some(false));
        assert_eq!(revision(&app), before);
        assert_eq!(
            app.world().resource::<ReferenceEnableState>().recomputed,
            work
        );
        assert_eq!(app.world().resource::<Changed>().0, notifications);
    }

    #[test]
    fn session_overrides_survive_streaming_and_same_catalog_bind() {
        let catalog = catalog("(1,2048,NULL,0)");
        let mut app = app(Arc::clone(&catalog));
        let root = app
            .world_mut()
            .spawn((FormId(1), Visibility::Inherited))
            .id();
        set(&mut app, 1, true);
        app.world_mut().despawn(root);
        let before = revision(&app);
        app.world_mut()
            .resource_mut::<ReferenceEnableState>()
            .bind(catalog);
        assert_eq!(revision(&app), before);
        let root = app.world_mut().spawn((FormId(1), Visibility::Visible)).id();
        assert_eq!(
            app.world().get::<Visibility>(root),
            Some(&Visibility::Visible)
        );
        set(&mut app, 1, false);
        assert_eq!(
            app.world().get::<Visibility>(root),
            Some(&Visibility::Hidden)
        );
    }

    #[test]
    fn different_catalog_resets_overrides_and_preserves_live_root_registry() {
        let mut app = app(catalog("(1,2048,NULL,0)"));
        let root = app
            .world_mut()
            .spawn((FormId(1), Visibility::Inherited))
            .id();
        set(&mut app, 1, true);
        app.world_mut()
            .resource_mut::<ReferenceEnableState>()
            .bind(catalog("(1,0,NULL,0)"));
        assert_eq!(enabled(&app, 1), Some(true));
        set(&mut app, 1, false);
        assert_eq!(
            app.world().get::<Visibility>(root),
            Some(&Visibility::Hidden)
        );
        set(&mut app, 1, true);
        assert_eq!(
            app.world().get::<Visibility>(root),
            Some(&Visibility::Inherited)
        );
    }

    #[test]
    fn recertified_equal_inputs_preserve_overrides_and_changed_inputs_reconcile_surviving_roots() {
        let mut app = app(catalog("(1,0,NULL,0)"));
        let root = app.world_mut().spawn((FormId(1), Visibility::Visible)).id();
        set(&mut app, 1, false);
        let before = revision(&app);
        assert!(
            !app.world_mut()
                .resource_mut::<ReferenceEnableState>()
                .bind(catalog("(1,0,NULL,0)"))
        );
        assert_eq!(enabled(&app, 1), Some(false));
        assert_eq!(revision(&app), before);
        assert!(
            app.world_mut()
                .resource_mut::<ReferenceEnableState>()
                .bind(catalog("(1,0,NULL,0),(2,0,NULL,0)"))
        );
        reconcile_bound_roots(app.world_mut());
        assert_eq!(enabled(&app, 1), Some(true));
        assert_eq!(
            app.world().get::<Visibility>(root),
            Some(&Visibility::Visible)
        );
    }

    #[test]
    fn missing_parents_cycles_and_unknown_ids_fail_closed_until_explicit_resolution() {
        let mut app = app(catalog("(1,0,99,1),(2,0,3,0),(3,0,2,1),(4,0,1,1)"));
        for reference in [1, 2, 3, 4, 99] {
            assert_eq!(enabled(&app, reference), None);
            let root = app
                .world_mut()
                .spawn((FormId(reference), Visibility::Visible))
                .id();
            assert_eq!(
                app.world().get::<Visibility>(root),
                Some(&Visibility::Hidden)
            );
        }
        let before = revision(&app);
        set(&mut app, 99, true);
        assert_eq!(revision(&app), before);
        assert_eq!(enabled(&app, 99), None);
        set(&mut app, 1, true);
        assert_eq!(enabled(&app, 1), Some(true));
        assert_eq!(enabled(&app, 4), Some(false));
        set(&mut app, 2, false);
        assert_eq!(enabled(&app, 2), Some(false));
        assert_eq!(enabled(&app, 3), Some(true));
    }

    #[test]
    fn player_bootstrap_can_be_overridden_without_a_database_row() {
        let mut app = app(catalog("(1,2048,20,0),(2,0,20,1)"));
        assert_eq!(enabled(&app, PLAYER_REFERENCE), Some(true));
        set(&mut app, PLAYER_REFERENCE, false);
        assert_eq!(enabled(&app, PLAYER_REFERENCE), Some(false));
        assert_eq!(enabled(&app, 1), Some(false));
        assert_eq!(enabled(&app, 2), Some(true));
    }

    #[test]
    fn roots_restore_their_own_prior_visibility_including_disabled_entities() {
        let mut app = app(catalog("(1,0,NULL,0)"));
        let roots: Vec<_> = [
            Visibility::Visible,
            Visibility::Inherited,
            Visibility::Hidden,
        ]
        .into_iter()
        .map(|visibility| {
            (
                app.world_mut()
                    .spawn((FormId(1), visibility, Disabled))
                    .id(),
                visibility,
            )
        })
        .collect();
        set(&mut app, 1, false);
        for (entity, _) in &roots {
            assert_eq!(
                app.world().get::<Visibility>(*entity),
                Some(&Visibility::Hidden)
            );
        }
        set(&mut app, 1, true);
        for (entity, visibility) in roots {
            assert_eq!(app.world().get::<Visibility>(entity), Some(&visibility));
        }
    }

    #[test]
    fn disabled_reference_root_gates_real_descendant_and_flat_draw_visibility() {
        use crate::cluster::flat_source::FlatSourceOwner;
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            bevy::asset::AssetPlugin::default(),
            bevy::transform::TransformPlugin,
            bevy::camera::visibility::VisibilityPlugin,
        ))
        .init_asset::<Mesh>()
        .init_asset::<bevy::mesh::skinning::SkinnedMeshInverseBindposes>();
        install(&mut app);
        app.world_mut()
            .resource_mut::<ReferenceEnableState>()
            .bind(catalog("(1,0,NULL,0)"));
        let root = app
            .world_mut()
            .spawn((FormId(1), Transform::default(), Visibility::Inherited))
            .id();
        let body = app
            .world_mut()
            .spawn((Mesh3d::default(), Visibility::Visible, ChildOf(root)))
            .id();
        let flat = app
            .world_mut()
            .spawn((
                Mesh3d::default(),
                Visibility::Visible,
                FlatSourceOwner(root),
            ))
            .id();
        app.update();
        assert!(app.world().get::<InheritedVisibility>(body).unwrap().get());
        app.world_mut().entity_mut(root).insert(Disabled);
        set(&mut app, 1, false);
        app.update();
        for draw in [body, flat] {
            assert_eq!(
                app.world().get::<Visibility>(draw),
                Some(&Visibility::Hidden)
            );
            assert!(!app.world().get::<InheritedVisibility>(draw).unwrap().get());
        }
        let late = app
            .world_mut()
            .spawn((Mesh3d::default(), Visibility::Visible, ChildOf(root)))
            .id();
        app.update();
        assert!(!app.world().get::<InheritedVisibility>(late).unwrap().get());
        set(&mut app, 1, true);
        app.update();
        for draw in [body, flat, late] {
            assert_eq!(
                app.world().get::<Visibility>(draw),
                Some(&Visibility::Visible)
            );
            assert!(app.world().get::<InheritedVisibility>(draw).unwrap().get());
        }
    }

    #[test]
    fn form_id_replacement_removal_and_despawn_cannot_leave_stale_visibility_writes() {
        let mut app = app(catalog("(1,2048,NULL,0),(2,0,NULL,0)"));
        let root = app.world_mut().spawn((FormId(1), Visibility::Visible)).id();
        assert_eq!(
            app.world().get::<Visibility>(root),
            Some(&Visibility::Hidden)
        );
        app.world_mut().entity_mut(root).insert(FormId(2));
        assert_eq!(
            app.world().get::<Visibility>(root),
            Some(&Visibility::Visible)
        );
        set(&mut app, 1, true);
        set(&mut app, 2, false);
        assert_eq!(
            app.world().get::<Visibility>(root),
            Some(&Visibility::Hidden)
        );
        app.world_mut().entity_mut(root).remove::<FormId>();
        assert_eq!(
            app.world().get::<Visibility>(root),
            Some(&Visibility::Visible)
        );
        set(&mut app, 2, true);
        set(&mut app, 2, false);
        assert_eq!(
            app.world().get::<Visibility>(root),
            Some(&Visibility::Visible)
        );
        app.world_mut().despawn(root);
        set(&mut app, 2, true);
    }

    #[test]
    fn registry_tracks_visibility_added_after_form_id_and_roots_before_binding() {
        let mut app = App::new();
        install(&mut app);
        assert!(!app.world().resource::<ReferenceEnableState>().is_bound());
        assert_eq!(enabled(&app, 1), None);
        let root = app.world_mut().spawn(FormId(1)).id();
        app.world_mut().entity_mut(root).insert(Visibility::Visible);
        app.world_mut()
            .resource_mut::<ReferenceEnableState>()
            .bind(catalog("(1,0,NULL,0)"));
        set(&mut app, 1, false);
        assert_eq!(
            app.world().get::<Visibility>(root),
            Some(&Visibility::Hidden)
        );
        app.world_mut().entity_mut(root).remove::<Visibility>();
        app.world_mut()
            .entity_mut(root)
            .insert(Visibility::Inherited);
        set(&mut app, 1, true);
        assert_eq!(
            app.world().get::<Visibility>(root),
            Some(&Visibility::Inherited)
        );
    }

    #[test]
    fn catalog_accepts_legacy_columns_and_rejects_partial_or_invalid_enable_metadata() {
        let legacy = Connection::open_in_memory().unwrap();
        legacy.execute_batch("CREATE TABLE \"references\" (id INTEGER PRIMARY KEY); INSERT INTO \"references\" VALUES (1);").unwrap();
        let mut state = ReferenceEnableState::default();
        state.bind(Arc::new(EnableCatalog::read(&legacy).unwrap()));
        assert_eq!(state.enabled(1), Some(true));
        legacy
            .execute_batch("ALTER TABLE \"references\" ADD COLUMN header_flags INTEGER;")
            .unwrap();
        assert!(EnableCatalog::read(&legacy).is_err());
        for rows in [
            "(1,0,2,NULL)",
            "(1,-1,NULL,0)",
            "(1,0,-1,0)",
            "(1,0,NULL,-1)",
        ] {
            assert!(
                EnableCatalog::read(&database(rows)).is_err(),
                "accepted {rows}"
            );
        }
    }

    #[test]
    fn catalog_initial_results_are_immutable_when_runtime_state_changes() {
        let catalog = catalog("(1,2048,NULL,0),(2,0,1,1),(3,0,99,0)");
        assert_eq!(catalog.enabled(1), Some(false));
        assert_eq!(catalog.enabled(2), Some(true));
        assert_eq!(catalog.enabled(3), None);
        assert_eq!(catalog.enabled(99), None);
        assert_eq!(catalog.enabled(PLAYER_REFERENCE), Some(true));
        let mut app = app(Arc::clone(&catalog));
        set(&mut app, 1, true);
        assert_eq!(enabled(&app, 1), Some(true));
        assert_eq!(catalog.enabled(1), Some(false));
        assert_eq!(catalog.enabled(2), Some(true));
    }

    #[test]
    fn catalog_reads_all_references_once_and_runtime_does_not_query_database() {
        let connection = database("(1,2048,NULL,0),(2,0,1,0),(3,0,2,1)");
        let catalog = Arc::new(EnableCatalog::read(&connection).unwrap());
        connection
            .execute_batch("DROP TABLE \"references\";")
            .unwrap();
        let mut app = app(catalog);
        assert_eq!(enabled(&app, 2), Some(false));
        assert_eq!(enabled(&app, 3), Some(true));
        set(&mut app, 1, true);
        assert_eq!(enabled(&app, 2), Some(true));
        assert_eq!(enabled(&app, 3), Some(false));
    }

    #[derive(Component)]
    struct CustomHidden;

    fn custom_hidden_app() -> App {
        let mut app = App::new();
        app.world_mut()
            .register_disabling_component::<CustomHidden>();
        install(&mut app);
        app
    }

    #[test]
    fn default_filtered_reference_registers_and_obeys_public_enable_commands() {
        for indexed_before_marker in [false, true] {
            let mut app = custom_hidden_app();
            bind_and_reconcile(&mut app, catalog("(1,2048,NULL,0)"));
            let root = if indexed_before_marker {
                let root = app.world_mut().spawn((FormId(1), Visibility::Visible)).id();
                app.world_mut().entity_mut(root).insert(CustomHidden);
                root
            } else {
                let root = app.world_mut().spawn(CustomHidden).id();
                app.world_mut()
                    .entity_mut(root)
                    .insert((FormId(1), Visibility::Visible));
                root
            };
            app.world_mut().flush();
            assert!(
                app.world()
                    .resource::<ReferenceEnableState>()
                    .entities
                    .contains_key(&root),
                "an event-target reference must be indexed regardless of default query filters"
            );
            assert_eq!(
                app.world().get::<Visibility>(root),
                Some(&Visibility::Hidden)
            );
            set(&mut app, 1, true);
            assert_eq!(
                app.world().get::<Visibility>(root),
                Some(&Visibility::Visible)
            );
            set(&mut app, 1, false);
            app.world_mut().entity_mut(root).remove::<CustomHidden>();
            assert_eq!(
                app.world().get::<Visibility>(root),
                Some(&Visibility::Hidden)
            );
            set(&mut app, 1, true);
            assert_eq!(
                app.world().get::<Visibility>(root),
                Some(&Visibility::Visible)
            );
        }
    }

    #[test]
    fn default_filtered_reference_replacement_restores_its_latest_authored_visibility() {
        let mut app = custom_hidden_app();
        bind_and_reconcile(&mut app, catalog("(1,2048,NULL,0),(2,0,NULL,0)"));
        let root = app.world_mut().spawn((FormId(1), Visibility::Visible)).id();
        app.world_mut().entity_mut(root).insert(CustomHidden);
        app.world_mut()
            .entity_mut(root)
            .insert(Visibility::Inherited);
        assert_eq!(
            app.world().get::<Visibility>(root),
            Some(&Visibility::Hidden)
        );
        set(&mut app, 1, true);
        assert_eq!(
            app.world().get::<Visibility>(root),
            Some(&Visibility::Inherited),
            "replacement visibility, not the old saved value, is the authored policy"
        );
        set(&mut app, 1, false);
        app.world_mut().entity_mut(root).insert(FormId(2));
        assert_eq!(
            app.world().get::<Visibility>(root),
            Some(&Visibility::Inherited)
        );
        app.world_mut().entity_mut(root).remove::<FormId>();
        assert!(
            !app.world()
                .resource::<ReferenceEnableState>()
                .entities
                .contains_key(&root)
        );
        set(&mut app, 1, true);
        assert_eq!(
            app.world().get::<Visibility>(root),
            Some(&Visibility::Inherited)
        );
    }
}
