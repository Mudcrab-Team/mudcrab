//! The render-to-main native queued/readiness bridge.
//!
//! Publishes native mesh/material/image readiness and same-view queue/pipeline proof to
//! main-world consumers. A completed CPU Render observation does not certify GPU execution
//! or visible pixels; each consumer owns its geometry lifetime.
//!
//! Extract publishes only stored results from the completed Render pass, then captures waiting
//! declarations. Render evaluates those inputs after RenderSystems::Render and before Cleanup.
//! Native entity probes cost O(waiting entities), with view/phase lookups per waiting entity.
//! Declaration writers withdraw old evidence atomically with their payload edit. Settled
//! declarations have no discovery scan or native resource probes.

use bevy::pbr::SpecializedMaterialPipelineCache;
use bevy::{
    asset::{AssetId, UntypedAssetId},
    ecs::{
        change_detection::Tick,
        system::{SystemParam, SystemState},
    },
    prelude::*,
    render::{
        ExtractSchedule, MainWorld, Render, RenderApp, RenderSystems,
        render_asset::RenderAssets,
        render_resource::{CachedPipelineState, PipelineCache},
        sync_world::MainEntity,
        texture::GpuImage,
        view::{ExtractedView, RenderVisibleEntities, RenderVisibleEntitiesClass},
    },
};
use std::collections::HashMap;

/// A surface waiting for completed native render readiness evidence.
///
/// This is a bare marker so the settle and the door prewarm can read one query over it. An entity
/// that carries it must also carry its [`SwapSurface`] declaration and a [`Mesh3d`]: the bridge's
/// query is the marker joined with those, and a marked entity without them is never visited.
///
/// The main-world hand-over removes the marker (and only the marker) the frame the render world's
/// confirmation arrives; a surface whose entity is despawned - a cell leaving the stream window -
/// leaves the set with it.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct SwapWaiting;

/// A waiting surface in a camera's render-visible mesh set in the last published completed snapshot.
/// Shadow-only visibility cannot release or block a camera destination.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct SwapInCameraView;

/// This waiting declaration has a matching completed camera-membership observation.
/// Absence is unknown, rather than evidence that the surface is offscreen.
#[derive(Component, Debug, Clone, Copy)]
pub(crate) struct SwapCameraObserved;

/// One readiness rule shared by global settling and per-cell destination checks.
#[derive(SystemParam)]
pub(crate) struct WaitingVisibility<'w, 's> {
    waiting: Query<'w, 's, (Has<SwapInCameraView>, Has<SwapCameraObserved>), With<SwapWaiting>>,
    results: Option<Res<'w, systems::RenderResults>>,
}

impl WaitingVisibility<'_, '_> {
    fn renderer_exists(&self) -> bool {
        self.results
            .as_deref()
            .is_some_and(|results| results.renderer)
    }

    pub(crate) fn pending_under(&self, entity: Entity) -> bool {
        self.renderer_exists()
            && self
                .waiting
                .get(entity)
                .is_ok_and(|(in_view, observed)| in_view || !observed)
    }
}

/// Visible or still-unobserved waiting surfaces. Headless runs have no render obligation.
pub(crate) fn pending_drawn(waiting: &WaitingVisibility) -> usize {
    if !waiting.renderer_exists() {
        return 0;
    }
    waiting
        .waiting
        .iter()
        .filter(|(in_view, observed)| *in_view || !observed)
        .count()
}

/// What a waiting surface's draw needs from the render world, as asset ids.
///
/// The declaration is the surface owner's (today `streaming::spawn_cell` for a terrain quadrant,
/// from the same layer images it hands to `TerrainExtension`) and it is kept true: when the owner
/// changes what the material binds, it re-declares the surface. It carries no terrain, cell or
/// worldspace: the same component serves a terrain quadrant, a LOD patch or any later "swap A for B
/// once B is drawn". Mesh/material/image contents are immutable for one declaration. Re-declaring
/// revokes old snapshots/markers but does not prove same-ID asset content freshness; that needs
/// the asset writer's versioned invalidation contract. Unannounced GPU writes are unsupported.
#[derive(Component, Debug, Clone)]
pub struct SwapSurface {
    /// The material asset the entity draws with, of whatever material type.
    material: UntypedAssetId,
    /// Every image the material's bind group holds, deduplicated.
    images: Vec<AssetId<Image>>,
}

impl SwapSurface {
    /// A surface's draw needs: its material and every image that material binds.
    ///
    /// Repeating an image (a quadrant can sample one texture in two layers) costs the bridge one
    /// lookup per *distinct* asset per frame, so the images are sorted and deduplicated here, once.
    pub fn new(material: UntypedAssetId, images: impl IntoIterator<Item = AssetId<Image>>) -> Self {
        let mut images: Vec<AssetId<Image>> = images.into_iter().collect();
        images.sort_unstable();
        images.dedup();
        Self { material, images }
    }

    /// Re-declares the images the material binds, keeping the material. The owner
    /// calls this when it changes the material in a way that removes a binding, and calls
    /// `rearm_swap` in that same World transaction to withdraw prior native evidence.
    pub fn set_images(&mut self, images: impl IntoIterator<Item = AssetId<Image>>) {
        let mut images: Vec<AssetId<Image>> = images.into_iter().collect();
        images.sort_unstable();
        images.dedup();
        self.images = images;
    }
}

/// A completed CPU Render observation held the declared images, matching native mesh/material
/// instances, mesh allocation/render asset, and the native material allocator slab/bind group.
/// Pending surfaces are rechecked every Render.
#[derive(Component, Debug, Clone, Copy)]
pub struct SwapPrepared {
    /// The bridge's own extract-frame index at which the surface was prepared. Kept for later
    /// lag gauges; nothing reads it yet.
    pub since: u32,
}

/// A completed CPU Render pass observed the declared payload ready and, in one camera view,
/// render visibility, exact native forward-phase membership and an `Ok` specialized pipeline.
/// This queued/readiness certificate does not prove draw-command execution, GPU execution or pixels.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct SwapDrawn;

/// What the completed CPU Render pass held for one waiting surface: the plain snapshot
/// [`swap_readiness`] decides on. Every field is a fact this world has established, so the decision
/// is a pure function of the snapshot and can be tested without a GPU.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SwapSurfaceFacts {
    /// The native mesh has an allocator slab mapping (`MeshAllocator::mesh_slabs`).
    /// This does not establish every vertex/index slice guard of `DrawMesh`.
    pub mesh_allocated: bool,
    /// The mesh's render form is prepared (`RenderAssets<RenderMesh>`).
    pub render_mesh: bool,
    /// How many of the declared images are prepared (`RenderAssets<GpuImage>`).
    pub images_ready: usize,
    /// How many images the surface declared. Zero means this surface needs no image prepared.
    pub images_declared: usize,
    /// Native material instance → prepared material → type allocator → slab → bind group exists.
    pub material: bool,
    /// Extracted native mesh and material instance IDs match this declaration.
    pub payload_matches: bool,
    /// A camera that sees this entity contains it in an exact native forward phase.
    pub forward_queued: bool,
    /// The same visible, forward-queued camera has a compiled pipeline for the entity
    /// (`SpecializedMaterialPipelineCache` + `PipelineCache`). False for an entity no view can see -
    /// which is why a surface nobody looks at stays prepared, never drawn.
    pub pipeline_ok: bool,
}

/// What the bridge decided about one surface in a completed CPU Render snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapReadiness {
    /// Something the draw needs is not in the render world yet.
    Waiting,
    /// Assets and matching native instances are ready; the same-view queue/pipeline proof is not.
    Prepared,
    /// Matching native payload is ready and a visible camera has both queue membership and pipeline.
    /// This is CPU readiness, not an executed GPU draw.
    Drawn,
}

/// Matching native payload, geometry, material slab/bind group and declared images establish
/// preparation. Exact forward membership and a compiled pipeline in the same visible camera
/// additionally establish the CPU queued/readiness certificate consumed as `SwapDrawn`.
pub fn swap_readiness(facts: &SwapSurfaceFacts) -> SwapReadiness {
    if !facts.mesh_allocated
        || !facts.render_mesh
        || facts.images_ready < facts.images_declared
        || !facts.material
        || !facts.payload_matches
    {
        return SwapReadiness::Waiting;
    }
    if facts.forward_queued && facts.pipeline_ok {
        SwapReadiness::Drawn
    } else {
        SwapReadiness::Prepared
    }
}

/// What a profiling gauge would read: how many swaps the main-world hand-over completed in the frame that
/// just ran. A resource because the hand-over removes the markers through commands, so a query
/// cannot see the frame's own completions (see [`complete_drawn_swaps`]).
#[derive(Resource, Debug, Default)]
pub struct SwapBridgeStats {
    /// Entities that carried both the waiting and the drawn marker this frame and left the
    /// waiting set.
    pub drawn_this_frame: u32,
}

/// Only waiting declarations are captured; settled entities never cause native resource probes.
#[allow(clippy::type_complexity)]
type WaitingSurfaces = Query<
    'static,
    'static,
    (
        Entity,
        Ref<'static, Mesh3d>,
        Ref<'static, SwapSurface>,
        Ref<'static, SwapWaiting>,
        Option<&'static SwapPrepared>,
        Has<SwapInCameraView>,
    ),
    (With<SwapWaiting>, Without<SwapDrawn>),
>;

/// Exact live declaration and waiting obligation, including native change ticks. IDs alone cannot
/// distinguish re-declaration with the same handles. Entity includes its generation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SwapDeclaration {
    entity: Entity,
    mesh: AssetId<Mesh>,
    material: UntypedAssetId,
    images: Vec<AssetId<Image>>,
    mesh_changed: Tick,
    surface_changed: Tick,
    waiting_added: Tick,
}

impl SwapDeclaration {
    fn capture(
        entity: Entity,
        mesh: &Ref<Mesh3d>,
        surface: &Ref<SwapSurface>,
        waiting: &Ref<SwapWaiting>,
    ) -> Self {
        Self {
            entity,
            mesh: mesh.0.id(),
            material: surface.material,
            images: surface.images.clone(),
            mesh_changed: mesh.last_changed(),
            surface_changed: surface.last_changed(),
            waiting_added: waiting.added(),
        }
    }
}

struct CompletedSwap {
    declaration: SwapDeclaration,
    facts: SwapSurfaceFacts,
    in_view: bool,
}

/// Private one-extract/one-Render buffers, bounded by that extract's waiting declarations.
/// Results are consumed once and replaced each Render; no settled/world history is retained.
#[derive(Resource, Default)]
struct SwapBuffers {
    inputs: Vec<SwapDeclaration>,
    completed: HashMap<Entity, CompletedSwap>,
}

/// Publish only the prior completed snapshot for the identical live waiting declaration, then
/// capture this extract's requests. No native render resource is inspected during Extract.
fn swap_bridge(
    mut main_world: ResMut<MainWorld>,
    mut waiting: Local<Option<SystemState<WaitingSurfaces>>>,
    mut frame: Local<u32>,
    mut decisions: Local<Vec<SwapDecision>>,
    mut buffers: ResMut<SwapBuffers>,
) {
    *frame = frame.saturating_add(1);
    let state = waiting.get_or_insert_with(|| SystemState::new(&mut main_world));
    decisions.clear();
    buffers.inputs.clear();
    {
        let query = state
            .get(&main_world)
            .expect("the query was built from this main world");
        for (entity, mesh, surface, obligation, prepared, in_view) in query.iter() {
            let declaration = SwapDeclaration::capture(entity, &mesh, &surface, &obligation);
            let result = buffers
                .completed
                .remove(&entity)
                .filter(|result| result.declaration == declaration);
            let mut drawn = false;
            if let Some(result) = result {
                let readiness = swap_readiness(&result.facts);
                drawn = readiness == SwapReadiness::Drawn;
                decisions.push(SwapDecision {
                    entity,
                    insert_prepared: readiness != SwapReadiness::Waiting && prepared.is_none(),
                    remove_prepared: readiness == SwapReadiness::Waiting && prepared.is_some(),
                    drawn,
                    in_view: (result.in_view != in_view).then_some(result.in_view),
                });
            }
            if !drawn {
                buffers.inputs.push(declaration);
            }
        }
    }
    // Anything not joined above was despawned, cancelled, changed or already drawn.
    buffers.completed.clear();
    for decision in decisions.iter() {
        let mut entity = main_world.entity_mut(decision.entity);
        // Every decision came from a completed snapshot for this exact declaration.
        if !entity.contains::<SwapCameraObserved>() {
            entity.insert(SwapCameraObserved);
        }
        if decision.remove_prepared {
            entity.remove::<SwapPrepared>();
        }
        if decision.insert_prepared {
            entity.insert(SwapPrepared { since: *frame });
        }
        if decision.drawn {
            entity.insert(SwapDrawn);
        }
        match decision.in_view {
            Some(true) => {
                entity.insert(SwapInCameraView);
            }
            Some(false) => {
                entity.remove::<SwapInCameraView>();
            }
            None => {}
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct SwapDecision {
    entity: Entity,
    insert_prepared: bool,
    remove_prepared: bool,
    drawn: bool,
    in_view: Option<bool>,
}

/// Observe native CPU readiness after Render has completed, before Cleanup discards frame state.
/// This does not observe individual draw-command invocation or GPU execution.
fn evaluate_swaps(
    mut buffers: ResMut<SwapBuffers>,
    draw: systems::DrawResources,
    queues: systems::QueuePhases,
    gpu_images: Res<RenderAssets<GpuImage>>,
    specialized: Option<Res<SpecializedMaterialPipelineCache>>,
    pipelines: Res<PipelineCache>,
    views: Query<(&ExtractedView, &RenderVisibleEntities)>,
) {
    let buffers = &mut *buffers;
    buffers.completed.clear();
    if buffers.inputs.is_empty() {
        return;
    }
    for declaration in buffers.inputs.drain(..) {
        let main = MainEntity::from(declaration.entity);
        let native = draw.entity_facts(main).0;
        let view = gather_view_facts(main, &views, &queues, specialized.as_deref(), &pipelines);
        let facts = SwapSurfaceFacts {
            mesh_allocated: native.mesh_allocated,
            render_mesh: native.render_mesh,
            images_ready: declaration
                .images
                .iter()
                .filter(|image| gpu_images.get(**image).is_some())
                .count(),
            images_declared: declaration.images.len(),
            material: native.material_prepared,
            payload_matches: draw.payload_matches(main, declaration.mesh, declaration.material),
            forward_queued: view.forward_queued,
            pipeline_ok: view.pipeline_ok,
        };
        buffers.completed.insert(
            declaration.entity,
            CompletedSwap {
                declaration,
                facts,
                in_view: view.in_view,
            },
        );
    }
}

#[derive(Default)]
struct SwapViewFacts {
    in_view: bool,
    forward_queued: bool,
    pipeline_ok: bool,
}

/// Keep visibility, native phase membership and specialization in one camera iteration. A queue
/// in one camera cannot satisfy a pipeline/visibility observation from another camera.
fn gather_view_facts(
    entity: MainEntity,
    views: &Query<(&ExtractedView, &RenderVisibleEntities)>,
    queues: &systems::QueuePhases,
    specialized: Option<&SpecializedMaterialPipelineCache>,
    pipelines: &PipelineCache,
) -> SwapViewFacts {
    let mut facts = SwapViewFacts::default();
    for (view, visible) in views.iter() {
        if !view_sees(visible, entity) {
            continue;
        }
        facts.in_view = true;
        if !queues.contains_forward(&view.retained_view_entity, entity) {
            continue;
        }
        facts.forward_queued = true;
        let cached = specialized
            .and_then(|cache| cache.get(&view.retained_view_entity))
            .and_then(|by_entity| by_entity.get(&entity));
        if cached.is_some_and(|cached| {
            matches!(
                pipelines.get_render_pipeline_state(*cached),
                CachedPipelineState::Ok(_)
            )
        }) {
            facts.pipeline_ok = true;
            break;
        }
    }
    facts
}

/// A payload writer withdraws old native evidence in the same World transaction as its edit.
/// Reinsert the obligation so a later extract cannot join an older waiting incarnation.
/// This is owner driven; no settled declaration is scanned or touched each frame.
pub(crate) fn rearm_swap(world: &mut World, entity: Entity) {
    if let Ok(mut entity) = world.get_entity_mut(entity)
        && entity.contains::<SwapSurface>()
    {
        entity.remove::<(
            SwapPrepared,
            SwapDrawn,
            SwapInCameraView,
            SwapCameraObserved,
            SwapWaiting,
        )>();
        entity.insert(SwapWaiting);
    }
}

/// Whether a view's visible set holds the entity: it is in the CPU-culled list (a sorted list, so
/// one binary search) or, for a mesh with `NoCpuCulling`, in the GPU-culled table.
fn view_sees(visible: &RenderVisibleEntities, entity: MainEntity) -> bool {
    visible
        .get::<Mesh3d>()
        .is_some_and(|class| class_sees(class, entity))
}

/// The membership test of [`view_sees`] against one view's mesh class.
fn class_sees(class: &RenderVisibleEntitiesClass, entity: MainEntity) -> bool {
    class.entities_gpu_culling.contains_key(&entity)
        || class
            .entities_cpu_culling
            .binary_search_by_key(&entity, |(_, main_entity)| *main_entity)
            .is_ok()
}

/// The main-world half of a swap: an entity the render world has confirmed drawn leaves the waiting
/// set. Runs in `Update`, so the marker the bridge wrote during the extract between two frames is
/// visible here, in the frame whose visibility pass follows it.
fn complete_drawn_swaps(
    mut commands: Commands,
    mut stats: ResMut<SwapBridgeStats>,
    drawn: Query<Entity, (With<SwapWaiting>, With<SwapDrawn>)>,
) {
    let mut completed = 0u32;
    for entity in &drawn {
        commands.entity(entity).try_remove::<SwapWaiting>();
        completed = completed.saturating_add(1);
    }
    stats.drawn_this_frame = completed;
}

/// Registers the render→main swap bridge: the extract system in the render app, the main-world
/// hand-over.
pub struct SwapBridgePlugin;

impl Plugin for SwapBridgePlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<systems::DestinationReadinessPlugin>() {
            app.add_plugins(systems::DestinationReadinessPlugin);
        }
        app.init_resource::<SwapBridgeStats>()
            .add_systems(Update, complete_drawn_swaps);
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app
                .init_resource::<SwapBuffers>()
                .add_systems(ExtractSchedule, swap_bridge)
                .add_systems(
                    Render,
                    evaluate_swaps
                        .after(RenderSystems::Render)
                        .before(RenderSystems::Cleanup),
                );
        }
    }
}

pub(crate) mod assets;
pub mod destination;
mod destination_evaluation;
pub mod systems;

#[cfg(test)]
mod tests {
    use super::*;

    fn surface(images: &[AssetId<Image>]) -> SwapSurface {
        SwapSurface::new(
            Handle::<StandardMaterial>::default().id().untyped(),
            images.iter().copied(),
        )
    }

    /// Every piece the render world has to hold for a surface to count as drawable: each one
    /// missing keeps the surface waiting, and all of them together make it drawn. The order of the
    /// two asset steps is the one the render world establishes them in
    /// (`allocate_and_free_meshes` before `prepare_assets::<RenderMesh>`), and the two cases are
    /// listed separately because a budget-deferred frame shows exactly that state.
    #[test]
    fn prepared_requires_every_asset() {
        let all = SwapSurfaceFacts {
            mesh_allocated: true,
            render_mesh: true,
            images_ready: 1,
            images_declared: 1,
            material: true,
            payload_matches: true,
            forward_queued: true,
            pipeline_ok: true,
        };
        assert_eq!(swap_readiness(&all), SwapReadiness::Drawn);

        for (what, facts, expected) in [
            (
                "no vertex buffers",
                SwapSurfaceFacts {
                    mesh_allocated: false,
                    ..all
                },
                SwapReadiness::Waiting,
            ),
            (
                "no RenderMesh",
                SwapSurfaceFacts {
                    render_mesh: false,
                    ..all
                },
                SwapReadiness::Waiting,
            ),
            (
                "an image not prepared",
                SwapSurfaceFacts {
                    images_ready: 0,
                    ..all
                },
                SwapReadiness::Waiting,
            ),
            (
                "no prepared material",
                SwapSurfaceFacts {
                    material: false,
                    ..all
                },
                SwapReadiness::Waiting,
            ),
            (
                "no compiled pipeline",
                SwapSurfaceFacts {
                    pipeline_ok: false,
                    ..all
                },
                SwapReadiness::Prepared,
            ),
        ] {
            assert_eq!(swap_readiness(&facts), expected, "{what}");
        }

        // A surface that declares no image needs none prepared: the count decides, not a bias
        // towards waiting.
        let image_free = SwapSurfaceFacts {
            images_ready: 0,
            images_declared: 0,
            ..all
        };
        assert_eq!(swap_readiness(&image_free), SwapReadiness::Drawn);
    }

    /// An entity whose pipeline is `Ok` but which is in no view's visible set is prepared, not
    /// drawn: the stand-in keeps covering it. This is the visibility half of the rule, and the
    /// bridge reads it from the view's own set rather than assuming the pipeline cache implies it.
    #[test]
    fn confirming_needs_a_view_that_can_see_it() {
        let mut world = World::new();
        let seen = world.spawn_empty().id();
        let gpu_seen = world.spawn_empty().id();
        let unseen = world.spawn_empty().id();

        let mut class = RenderVisibleEntitiesClass {
            entities_cpu_culling: vec![(Entity::PLACEHOLDER, MainEntity::from(seen))],
            ..Default::default()
        };
        class
            .entities_gpu_culling
            .insert(MainEntity::from(gpu_seen), Entity::PLACEHOLDER);

        assert!(class_sees(&class, MainEntity::from(seen)));
        assert!(class_sees(&class, MainEntity::from(gpu_seen)));
        assert!(!class_sees(&class, MainEntity::from(unseen)));
    }

    /// The one query the bridge runs visits the marked entities and nothing else, however many
    /// entities the world holds: this is the cost bound in its smallest form.
    #[test]
    fn the_bridge_visits_only_the_marked_entities() {
        let mut world = World::new();
        let mut marked = Vec::new();
        for _ in 0..4 {
            marked.push(
                world
                    .spawn((Mesh3d(Handle::<Mesh>::default()), surface(&[]), SwapWaiting))
                    .id(),
            );
        }
        // Entities that are everything but marked: meshes the scene holds, a surface declaration
        // with nothing waiting on it, and an entity carrying only the marker.
        for _ in 0..16 {
            world.spawn((Mesh3d(Handle::<Mesh>::default()), surface(&[])));
        }
        world.spawn(SwapWaiting);

        let mut state = SystemState::<WaitingSurfaces>::new(&mut world);
        let query = state
            .get(&world)
            .expect("the state was built from this world");
        let visited: Vec<Entity> = query.iter().map(|(entity, ..)| entity).collect();
        assert_eq!(visited, marked);
    }

    /// The hand-over: a surface whose drawn marker has arrived leaves the waiting set, and the
    /// count the gauge reads is that frame's completions only.
    #[test]
    fn the_hand_over_releases_a_drawn_surface() {
        let mut app = App::new();
        app.add_plugins(SwapBridgePlugin);
        let drawn = app.world_mut().spawn((SwapWaiting, SwapDrawn)).id();
        let waiting = app.world_mut().spawn(SwapWaiting).id();

        app.update();

        assert!(
            app.world().get::<SwapWaiting>(drawn).is_none(),
            "a drawn surface leaves the waiting set"
        );
        assert!(
            app.world().get::<SwapWaiting>(waiting).is_some(),
            "a surface still waiting keeps its marker"
        );
        assert_eq!(
            app.world().resource::<SwapBridgeStats>().drawn_this_frame,
            1
        );

        app.update();
        assert_eq!(
            app.world().resource::<SwapBridgeStats>().drawn_this_frame,
            0,
            "the count is the frame's own completions, not a running total"
        );
    }

    /// A surface's declared images are deduplicated, so the bridge pays one lookup per distinct
    /// image however many layers sample it; re-declaring them keeps the material and stand-in.
    #[test]
    fn a_surfaces_images_are_deduplicated_and_redeclared() {
        let mut world = World::new();
        world.init_resource::<Assets<Image>>();
        let (base, normal) = {
            let mut images = world.resource_mut::<Assets<Image>>();
            (images.add(Image::default()), images.add(Image::default()))
        };
        assert_ne!(base.id(), normal.id(), "two images, two ids");

        let material = Handle::<StandardMaterial>::default().id().untyped();
        let mut surface =
            SwapSurface::new(material, [base.id(), normal.id(), base.id(), normal.id()]);
        assert_eq!(
            surface.images,
            vec![base.id(), normal.id()],
            "each image once"
        );
        assert_eq!(surface.material, material);
        surface.set_images([normal.id()]);
        assert_eq!(surface.images, vec![normal.id()]);
        assert_eq!(surface.material, material);
    }

    fn native_swap_app() -> App {
        let mut app = App::new();
        app.add_plugins(SwapBridgePlugin);
        app
    }

    fn complete_facts() -> SwapSurfaceFacts {
        SwapSurfaceFacts {
            mesh_allocated: true,
            render_mesh: true,
            images_ready: 0,
            images_declared: 0,
            material: true,
            payload_matches: true,
            forward_queued: true,
            pipeline_ok: true,
        }
    }

    #[test]
    fn visible_compiled_but_absent_forward_phase_is_only_prepared() {
        let all = complete_facts();
        assert_eq!(swap_readiness(&all), SwapReadiness::Drawn);
        assert_eq!(
            swap_readiness(&SwapSurfaceFacts {
                forward_queued: false,
                ..all
            }),
            SwapReadiness::Prepared
        );
        assert_eq!(
            swap_readiness(&SwapSurfaceFacts {
                payload_matches: false,
                ..all
            }),
            SwapReadiness::Waiting
        );
        assert_eq!(
            swap_readiness(&SwapSurfaceFacts {
                material: false,
                ..all
            }),
            SwapReadiness::Waiting
        );
    }

    fn extraction_world() -> (World, Schedule, Entity) {
        let mut main = MainWorld::default();
        // This protocol fixture models a real renderer; headless readiness is vacuous.
        main.init_resource::<systems::RenderResults>();
        main.resource_mut::<systems::RenderResults>().renderer = true;
        let entity = main
            .spawn((Mesh3d(Handle::default()), surface(&[]), SwapWaiting))
            .id();
        let mut render = World::new();
        render.insert_resource(main);
        render.init_resource::<SwapBuffers>();
        let mut extract = Schedule::new(ExtractSchedule);
        extract.add_systems(swap_bridge);
        (render, extract, entity)
    }

    // Protocol-only fixture: this deliberately does not claim native rendering readiness.
    // Native phase/material tests below exercise the resources that produce these results.
    fn finish_snapshot(render: &mut World, facts: SwapSurfaceFacts) {
        let mut buffers = render.resource_mut::<SwapBuffers>();
        let inputs = std::mem::take(&mut buffers.inputs);
        for declaration in inputs {
            buffers.completed.insert(
                declaration.entity,
                CompletedSwap {
                    declaration,
                    facts,
                    in_view: true,
                },
            );
        }
    }

    #[test]
    fn extract_never_synthesizes_a_completed_render_snapshot() {
        let (mut render, mut extract, entity) = extraction_world();
        for _ in 0..3 {
            extract.run(&mut render);
            assert!(
                render
                    .resource::<MainWorld>()
                    .get::<SwapDrawn>(entity)
                    .is_none()
            );
            assert_eq!(render.resource::<SwapBuffers>().inputs.len(), 1);
        }
        finish_snapshot(&mut render, complete_facts());
        assert!(
            render
                .resource::<MainWorld>()
                .get::<SwapDrawn>(entity)
                .is_none()
        );
        extract.run(&mut render);
        assert!(
            render
                .resource::<MainWorld>()
                .get::<SwapDrawn>(entity)
                .is_some()
        );
        assert!(
            render
                .resource::<MainWorld>()
                .get::<SwapPrepared>(entity)
                .is_some()
        );
        assert!(render.resource::<SwapBuffers>().completed.is_empty());
    }

    #[test]
    fn stale_snapshot_rejects_same_ids_redeclared_images_and_changed_mesh() {
        let (mut render, mut extract, entity) = extraction_world();
        extract.run(&mut render);
        finish_snapshot(&mut render, complete_facts());
        {
            let mut main = render.resource_mut::<MainWorld>();
            main.increment_change_tick();
            main.get_mut::<SwapSurface>(entity).unwrap().set_images([]);
        }
        extract.run(&mut render);
        assert!(
            render
                .resource::<MainWorld>()
                .get::<SwapDrawn>(entity)
                .is_none()
        );
        assert!(
            render
                .resource::<MainWorld>()
                .get::<SwapPrepared>(entity)
                .is_none()
        );
        finish_snapshot(&mut render, complete_facts());
        {
            let mut main = render.resource_mut::<MainWorld>();
            main.increment_change_tick();
            main.entity_mut(entity).insert(Mesh3d(Handle::default()));
        }
        extract.run(&mut render);
        assert!(
            render
                .resource::<MainWorld>()
                .get::<SwapDrawn>(entity)
                .is_none()
        );
        finish_snapshot(&mut render, complete_facts());
        extract.run(&mut render);
        assert!(
            render
                .resource::<MainWorld>()
                .get::<SwapDrawn>(entity)
                .is_some()
        );
    }

    #[test]
    fn stale_snapshot_rejects_changed_payload_ids() {
        let (mut render, mut extract, entity) = extraction_world();
        for change in 0..3 {
            extract.run(&mut render);
            finish_snapshot(&mut render, complete_facts());
            {
                let mut main = render.resource_mut::<MainWorld>();
                main.increment_change_tick();
                match change {
                    0 => {
                        let mut meshes = Assets::<Mesh>::default();
                        let mesh = meshes.add(Mesh::from(Cuboid::default()));
                        main.entity_mut(entity).insert(Mesh3d(mesh));
                    }
                    1 => {
                        let mut materials = Assets::<StandardMaterial>::default();
                        let material = materials.add(StandardMaterial::default());
                        main.get_mut::<SwapSurface>(entity).unwrap().material =
                            material.id().untyped();
                    }
                    _ => {
                        let mut images = Assets::<Image>::default();
                        let image = images.add(Image::default());
                        main.get_mut::<SwapSurface>(entity)
                            .unwrap()
                            .set_images([image.id()]);
                    }
                }
            }
            extract.run(&mut render);
            assert!(
                render
                    .resource::<MainWorld>()
                    .get::<SwapDrawn>(entity)
                    .is_none()
            );
            assert!(
                render
                    .resource::<MainWorld>()
                    .get::<SwapPrepared>(entity)
                    .is_none()
            );
        }
    }

    #[test]
    fn removed_waiting_declaration_or_mesh_cannot_receive_a_snapshot() {
        for removal in 0..3 {
            let (mut render, mut extract, entity) = extraction_world();
            extract.run(&mut render);
            finish_snapshot(&mut render, complete_facts());
            {
                let mut main = render.resource_mut::<MainWorld>();
                let mut surface = main.entity_mut(entity);
                match removal {
                    0 => {
                        surface.remove::<SwapWaiting>();
                    }
                    1 => {
                        surface.remove::<SwapSurface>();
                    }
                    _ => {
                        surface.remove::<Mesh3d>();
                    }
                }
            }
            extract.run(&mut render);
            assert!(
                render
                    .resource::<MainWorld>()
                    .get::<SwapDrawn>(entity)
                    .is_none()
            );
            assert!(render.resource::<SwapBuffers>().inputs.is_empty());
            assert!(render.resource::<SwapBuffers>().completed.is_empty());
        }
    }

    #[test]
    fn removed_then_readded_waiting_and_entity_reuse_reject_old_snapshots() {
        let (mut render, mut extract, entity) = extraction_world();
        extract.run(&mut render);
        finish_snapshot(&mut render, complete_facts());
        {
            let mut main = render.resource_mut::<MainWorld>();
            main.entity_mut(entity).remove::<SwapWaiting>();
            main.increment_change_tick();
            main.entity_mut(entity).insert(SwapWaiting);
        }
        extract.run(&mut render);
        assert!(
            render
                .resource::<MainWorld>()
                .get::<SwapDrawn>(entity)
                .is_none()
        );
        finish_snapshot(&mut render, complete_facts());
        let replacement = {
            let mut main = render.resource_mut::<MainWorld>();
            main.despawn(entity);
            main.spawn((Mesh3d(Handle::default()), surface(&[]), SwapWaiting))
                .id()
        };
        assert_ne!(entity, replacement);
        extract.run(&mut render);
        assert!(
            render
                .resource::<MainWorld>()
                .get::<SwapDrawn>(replacement)
                .is_none()
        );
    }

    #[test]
    fn native_failure_does_not_reuse_prepared_asset_booleans() {
        let (mut render, mut extract, entity) = extraction_world();
        render
            .resource_mut::<MainWorld>()
            .entity_mut(entity)
            .insert(SwapPrepared { since: 1 });
        extract.run(&mut render);
        finish_snapshot(
            &mut render,
            SwapSurfaceFacts {
                material: false,
                ..complete_facts()
            },
        );
        extract.run(&mut render);
        assert!(
            render
                .resource::<MainWorld>()
                .get::<SwapDrawn>(entity)
                .is_none()
        );
        assert!(
            render
                .resource::<MainWorld>()
                .get::<SwapPrepared>(entity)
                .is_none()
        );
        finish_snapshot(&mut render, complete_facts());
        extract.run(&mut render);
        assert!(
            render
                .resource::<MainWorld>()
                .get::<SwapDrawn>(entity)
                .is_some()
        );
    }

    #[test]
    fn settled_empty_extract_clears_buffers_without_inspecting_native_resources() {
        let (mut render, mut extract, entity) = extraction_world();
        extract.run(&mut render);
        finish_snapshot(&mut render, complete_facts());
        render
            .resource_mut::<MainWorld>()
            .entity_mut(entity)
            .remove::<SwapWaiting>();
        extract.run(&mut render);
        assert!(render.resource::<SwapBuffers>().inputs.is_empty());
        assert!(render.resource::<SwapBuffers>().completed.is_empty());
        // No renderer resources were installed in this World at any point.
        extract.run(&mut render);
        assert!(render.resource::<SwapBuffers>().inputs.is_empty());
    }

    #[test]
    fn changed_declarations_rearm_before_completion() {
        for change_mesh in [false, true] {
            let mut app = native_swap_app();
            let entity = app
                .world_mut()
                .spawn((
                    Mesh3d(Handle::default()),
                    surface(&[]),
                    SwapWaiting,
                    SwapDrawn,
                    SwapInCameraView,
                    SwapPrepared { since: 1 },
                ))
                .id();
            // Declarations and marker installed together are not a later re-declaration.
            app.world_mut().run_schedule(Update);
            assert!(app.world().get::<SwapWaiting>(entity).is_none());
            app.world_mut().increment_change_tick();
            if change_mesh {
                app.world_mut()
                    .entity_mut(entity)
                    .insert(Mesh3d(Handle::default()));
            } else {
                app.world_mut()
                    .get_mut::<SwapSurface>(entity)
                    .unwrap()
                    .set_images([]);
            }
            rearm_swap(app.world_mut(), entity);
            app.update();
            assert!(app.world().get::<SwapWaiting>(entity).is_some());
            assert!(app.world().get::<SwapDrawn>(entity).is_none());
            assert!(app.world().get::<SwapPrepared>(entity).is_none());
            assert!(app.world().get::<SwapInCameraView>(entity).is_none());
            assert_eq!(
                app.world().resource::<SwapBridgeStats>().drawn_this_frame,
                0
            );
            // A new proof on the unchanged declaration still uses the original consumer.
            app.world_mut().entity_mut(entity).insert(SwapDrawn);
            app.update();
            assert!(app.world().get::<SwapWaiting>(entity).is_none());
        }
    }

    #[test]
    fn changed_pending_prepared_declaration_revokes_preparation_and_visibility() {
        let mut app = native_swap_app();
        let entity = app
            .world_mut()
            .spawn((
                Mesh3d(Handle::default()),
                surface(&[]),
                SwapWaiting,
                SwapInCameraView,
                SwapPrepared { since: 1 },
            ))
            .id();
        app.update();
        app.world_mut().increment_change_tick();
        app.world_mut()
            .get_mut::<SwapSurface>(entity)
            .unwrap()
            .set_images([]);
        rearm_swap(app.world_mut(), entity);
        app.update();
        assert!(app.world().get::<SwapWaiting>(entity).is_some());
        assert!(app.world().get::<SwapPrepared>(entity).is_none());
        assert!(app.world().get::<SwapInCameraView>(entity).is_none());
        assert!(app.world().get::<SwapDrawn>(entity).is_none());
    }

    #[test]
    fn unknown_camera_membership_holds_readiness_until_a_completed_observation() {
        fn pending(render: &mut World) -> usize {
            let mut main = render.resource_mut::<MainWorld>();
            let mut query = SystemState::<WaitingVisibility>::new(&mut main);
            pending_drawn(&query.get(&main).unwrap())
        }
        let (mut render, mut extract, entity) = extraction_world();
        assert_eq!(
            pending(&mut render),
            1,
            "absence of a camera observation is unknown, not offscreen"
        );
        extract.run(&mut render);
        assert_eq!(
            pending(&mut render),
            1,
            "extraction alone proves no camera membership"
        );
        finish_snapshot(
            &mut render,
            SwapSurfaceFacts {
                forward_queued: false,
                pipeline_ok: false,
                ..complete_facts()
            },
        );
        render
            .resource_mut::<SwapBuffers>()
            .completed
            .get_mut(&entity)
            .unwrap()
            .in_view = false;
        extract.run(&mut render);
        assert_eq!(
            pending(&mut render),
            0,
            "a completed offscreen observation may stop holding camera readiness"
        );
        let main = render.resource::<MainWorld>();
        assert!(main.get::<SwapWaiting>(entity).is_some());
        assert!(main.get::<SwapDrawn>(entity).is_none());
    }
}
