//! The render→main "prepared and drawable" bridge.
//!
//! The engine hides a stand-in (the LOD tile under a full-detail cell, the root a detail change
//! replaces) on a purely main-world readiness flag: `TerrainSurfaceReady` says the textures are in
//! `Assets<Image>`, not that the mesh, its images, the material's bind group and its pipeline exist
//! in the render world. The frame that flag appears the stand-in goes and the replacement may not
//! be drawn - which is the hole. This module makes "drawable" a fact the main world can read.
//!
//! The direction is the sanctioned one: the render world's `ExtractSchedule` reads the entities the
//! engine has marked and writes the outcome back into the main world, exactly as Bevy's own
//! render-asset diagnostics publish their counters (`bevy_render::diagnostic`).
//!
//! Nothing in this module hides, despawns, delays or settles anything. It only writes the markers;
//! a consumer decides what to do with them, such as holding a LOD tile until the surface above it
//! is confirmed drawn.
//!
//! Cost is O(waiting entities): the bridge iterates exactly the entities carrying the waiting
//! marker, never all terrain, and a surface leaves the set the frame it is confirmed drawn.

use bevy::pbr::{PreparedMaterial, SpecializedMaterialPipelineCache};
use bevy::{
    asset::{AssetId, UntypedAssetId},
    ecs::system::SystemState,
    prelude::*,
    render::{
        ExtractSchedule, MainWorld, RenderApp,
        erased_render_asset::ErasedRenderAssets,
        mesh::{RenderMesh, allocator::MeshAllocator},
        render_asset::RenderAssets,
        render_resource::{CachedPipelineState, PipelineCache},
        sync_world::MainEntity,
        texture::GpuImage,
        view::{ExtractedView, RenderVisibleEntities, RenderVisibleEntitiesClass},
    },
};

/// A surface that is waiting for the render world: the engine has spawned it, the stand-in that
/// covers it is still drawn, and nothing yet knows whether it can be drawn.
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

/// What a waiting surface's draw needs from the render world, as asset ids.
///
/// The declaration is the surface owner's (today `streaming::spawn_cell` for a terrain quadrant,
/// from the same layer images it hands to `TerrainExtension`) and it is kept true: when the owner
/// changes what the material binds, it re-declares the surface. It carries no terrain, cell or
/// worldspace: the same component serves a terrain quadrant, a LOD patch or any later "swap A for B
/// once B is drawn".
#[derive(Component, Debug, Clone)]
pub struct SwapSurface {
    /// The material asset the entity draws with, of whatever material type.
    material: UntypedAssetId,
    /// Every image the material's bind group holds, deduplicated.
    images: Vec<AssetId<Image>>,
    /// The entity that covers this surface until it is drawn (the LOD tile under the quadrant, or
    /// the root a detail change replaces), carried over into [`SwapPrepared`]. This change inserts
    /// no stand-in.
    stand_in: Option<Entity>,
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
        Self {
            material,
            images,
            stand_in: None,
        }
    }

    /// Re-declares the images the material binds, keeping the material and any stand-in. The owner
    /// calls this when it changes the material in a way that removes a binding.
    pub fn set_images(&mut self, images: impl IntoIterator<Item = AssetId<Image>>) {
        let mut images: Vec<AssetId<Image>> = images.into_iter().collect();
        images.sort_unstable();
        images.dedup();
        self.images = images;
    }

    /// The surface's stand-in, if it declares one.
    pub fn stand_in(&self) -> Option<Entity> {
        self.stand_in
    }
}

/// The render world has everything this surface's draw needs, seen in the extract *after* the
/// render frame that established it: the mesh buffers and `RenderMesh`, every image the material
/// binds, and the material's prepared bind group. The stand-in may yield to it; nothing in this
/// slice acts on that.
#[derive(Component, Debug, Clone, Copy)]
pub struct SwapPrepared {
    /// The stand-in this surface replaces, if it has one (see [`SwapSurface`]).
    pub stand_in: Option<Entity>,
    /// The bridge's own extract-frame index at which the surface was prepared. Kept for later
    /// lag gauges; nothing reads it yet.
    pub since: u32,
}

/// The render world has confirmed this surface drawable: its own specialized pipeline resolved
/// `Ok` while a view could see it, so that frame's queue pass drew it.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct SwapDrawn;

/// What the render world holds for one waiting surface, this extract: the plain snapshot
/// [`swap_readiness`] decides on. Every field is a fact this world has established, so the decision
/// is a pure function of the snapshot and can be tested without a GPU.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SwapSurfaceFacts {
    /// The mesh's vertex/index buffers exist in the GPU allocator (`MeshAllocator::mesh_slabs`).
    pub mesh_allocated: bool,
    /// The mesh's render form is prepared (`RenderAssets<RenderMesh>`).
    pub render_mesh: bool,
    /// How many of the declared images are prepared (`RenderAssets<GpuImage>`).
    pub images_ready: usize,
    /// How many images the surface declared. Zero means this surface needs no image prepared.
    pub images_declared: usize,
    /// The material's bind group exists (`ErasedRenderAssets<PreparedMaterial>`), which is the
    /// lookup that fails while any image the material binds is still missing.
    pub material: bool,
    /// A view that can see the entity also has a compiled pipeline for it
    /// (`SpecializedMaterialPipelineCache` + `PipelineCache`). False for an entity no view can see -
    /// which is why a surface nobody looks at stays prepared, never drawn.
    pub pipeline_ok: bool,
}

impl SwapSurfaceFacts {
    /// The facts a surface that is *already* prepared reads: the render world holds its buffers, its
    /// mesh, its material and every image it binds, and only the pipeline can still change.
    ///
    /// Re-reading the asset lookups for a prepared surface would cost the per-frame lookups, which
    /// the design bounds to one or two per item. They cannot un-prepare while the asset is
    /// unchanged, and the marker's contract is that a surface's declared assets are set once, at
    /// the moment it is marked waiting.
    pub const fn assets_ready() -> Self {
        Self {
            mesh_allocated: true,
            render_mesh: true,
            images_ready: 0,
            images_declared: 0,
            material: true,
            pipeline_ok: false,
        }
    }
}

/// What the bridge decided about one surface this extract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapReadiness {
    /// Something the draw needs is not in the render world yet.
    Waiting,
    /// Everything is there; the pipeline is not (or no view can see it yet).
    Prepared,
    /// Everything is there and a pipeline that can draw it is compiled.
    Drawn,
}

/// The rule, in one place: a surface is [`SwapReadiness::Prepared`] when the
/// render world holds its mesh buffers, its `RenderMesh`, every image its material binds and the
/// material's prepared bind group, and [`SwapReadiness::Drawn`] once, in addition, a view that can
/// see it has a compiled pipeline for it. Anything missing keeps it [`SwapReadiness::Waiting`], and
/// the marker's absence from the waiting set at the end of a frame is the drawn signal.
pub fn swap_readiness(facts: &SwapSurfaceFacts) -> SwapReadiness {
    if !facts.mesh_allocated
        || !facts.render_mesh
        || facts.images_ready < facts.images_declared
        || !facts.material
    {
        return SwapReadiness::Waiting;
    }
    if facts.pipeline_ok {
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

/// The bridge's one main-world query: the entities the engine has marked waiting, with what their
/// draw needs. The cached [`SystemState`] is built from the main world on the first extract (the
/// pattern `bevy_render`'s `extract_screenshots` uses) and kept for the run.
///
/// The `With<SwapWaiting>` filter is the whole cost bound: the bridge visits this set and nothing
/// else, however many entities the world holds.
#[allow(clippy::type_complexity)]
type WaitingSurfaces = Query<
    'static,
    'static,
    (
        Entity,
        &'static Mesh3d,
        &'static SwapSurface,
        Option<&'static SwapPrepared>,
    ),
    With<SwapWaiting>,
>;

/// The bridge: for every waiting surface, the render world's own answer to "is this drawable?".
///
/// It reads the state the *previous* render frame left - assets are prepared and pipelines are
/// compiled in the render world's `Render` schedule, after the extract - and writes a marker into
/// the main world, which sees it on its next frame. A surface is up in the stand-in for at least
/// those frames, so no frame shows neither.
#[allow(clippy::too_many_arguments)]
fn swap_bridge(
    mut main_world: ResMut<MainWorld>,
    mut waiting: Local<Option<SystemState<WaitingSurfaces>>>,
    mut frame: Local<u32>,
    mut decisions: Local<Vec<SwapDecision>>,
    mesh_allocator: Res<MeshAllocator>,
    render_meshes: Res<RenderAssets<RenderMesh>>,
    gpu_images: Res<RenderAssets<GpuImage>>,
    materials: Res<ErasedRenderAssets<PreparedMaterial>>,
    specialized: Option<Res<SpecializedMaterialPipelineCache>>,
    pipelines: Res<PipelineCache>,
    views: Query<(&ExtractedView, &RenderVisibleEntities)>,
) {
    // The pipeline cache is created in the render world's `RenderStartup`; before it has run
    // nothing has been drawn, so there is nothing to confirm.
    let Some(specialized) = specialized else {
        return;
    };

    *frame = frame.saturating_add(1);
    if waiting.is_none() {
        *waiting = Some(SystemState::new(&mut main_world));
    }
    let state = waiting.as_mut().expect("the state was created just above");

    decisions.clear();
    {
        let query = state
            .get(&main_world)
            .expect("the cached query state was built from the main world");
        for (entity, mesh, surface, prepared) in query.iter() {
            let facts = if prepared.is_some() {
                SwapSurfaceFacts::assets_ready()
            } else {
                gather_surface_facts(
                    mesh,
                    surface,
                    &mesh_allocator,
                    &render_meshes,
                    &gpu_images,
                    &materials,
                )
            };
            let facts = SwapSurfaceFacts {
                pipeline_ok: pipeline_resolved(
                    MainEntity::from(entity),
                    &views,
                    &specialized,
                    &pipelines,
                ),
                ..facts
            };
            match swap_readiness(&facts) {
                SwapReadiness::Waiting => {}
                readiness => decisions.push(SwapDecision {
                    entity,
                    insert_prepared: prepared.is_none(),
                    drawn: readiness == SwapReadiness::Drawn,
                    stand_in: surface.stand_in(),
                }),
            }
        }
    }

    for decision in decisions.iter() {
        let mut entity = main_world.entity_mut(decision.entity);
        if decision.insert_prepared {
            entity.insert(SwapPrepared {
                stand_in: decision.stand_in,
                since: *frame,
            });
        }
        if decision.drawn {
            entity.insert(SwapDrawn);
        }
    }
}

/// One marker write the bridge decided on, applied after the query's borrow of the main world ends.
#[derive(Debug, Clone, Copy)]
struct SwapDecision {
    entity: Entity,
    /// Whether `SwapPrepared` still has to be written (it need not be re-written every frame a
    /// surface waits for its pipeline: that would churn change detection for nothing).
    insert_prepared: bool,
    drawn: bool,
    stand_in: Option<Entity>,
}

/// Reads the render world's state for one surface: the five lookups of the swap design minus the
/// pipeline, which depends on the views and is read separately.
fn gather_surface_facts(
    mesh: &Mesh3d,
    surface: &SwapSurface,
    mesh_allocator: &MeshAllocator,
    render_meshes: &RenderAssets<RenderMesh>,
    gpu_images: &RenderAssets<GpuImage>,
    materials: &ErasedRenderAssets<PreparedMaterial>,
) -> SwapSurfaceFacts {
    let mesh_id = mesh.0.id();
    SwapSurfaceFacts {
        mesh_allocated: mesh_allocator.mesh_slabs(&mesh_id).is_some(),
        render_mesh: render_meshes.get(mesh_id).is_some(),
        images_ready: surface
            .images
            .iter()
            .filter(|image| gpu_images.get(**image).is_some())
            .count(),
        images_declared: surface.images.len(),
        material: materials.get(surface.material).is_some(),
        pipeline_ok: false,
    }
}

/// Whether the entity's specialized pipeline for a view that can see it resolves `Ok`.
///
/// The pipeline cache is keyed by view and by main-world entity, and Bevy specializes an entity
/// only for the views whose visible set holds it - but the visible set is read here too, so the
/// "a view can see it" half of the rule is a fact of this frame rather than an assumption about
/// Bevy's internals. An entity whose pipeline is `Queued` (or absent) is not drawn yet.
fn pipeline_resolved(
    entity: MainEntity,
    views: &Query<(&ExtractedView, &RenderVisibleEntities)>,
    specialized: &SpecializedMaterialPipelineCache,
    pipelines: &PipelineCache,
) -> bool {
    for (view, visible) in views.iter() {
        // The cheap half first: a view with no cache entry or no pipeline for the entity cannot
        // have drawn it, and costs one lookup each.
        let Some(cached) = specialized
            .get(&view.retained_view_entity)
            .and_then(|by_entity| by_entity.get(&entity))
            .copied()
        else {
            continue;
        };
        if !view_sees(visible, entity) {
            continue;
        }
        if matches!(
            pipelines.get_render_pipeline_state(cached),
            CachedPipelineState::Ok(_)
        ) {
            return true;
        }
    }
    false
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
///
/// What a stand-in does with the release is left to a later change; this only releases the count.
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
        app.init_resource::<SwapBridgeStats>()
            .add_systems(Update, complete_drawn_swaps);
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app.add_systems(ExtractSchedule, swap_bridge);
        }
    }
}

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
        assert_eq!(surface.stand_in(), None);
    }
}
