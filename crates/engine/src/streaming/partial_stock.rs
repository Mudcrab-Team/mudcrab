//! Distant stock contains only permanently refused source occurrences.

#[cfg(test)]
use super::AssetFailed;
use super::{PendingAssetProfile, PendingModel};
use crate::{
    cluster::{ownership::OwnershipFacts, reader::CertifiedPackage},
    world::{
        components::{ExpectedModelBounds, InstanceBounds},
        database::ReferenceRow,
    },
};
use bevy::{
    asset::{AssetId, LoadState, RecursiveDependencyLoadState},
    camera::primitives::MeshAabb,
    prelude::*,
    world_serialization::WorldAssetRoot,
};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    sync::Arc,
};

#[derive(Component)]
pub(super) struct PartialStock {
    package: Arc<CertifiedPackage>,
    row: ReferenceRow,
    refused: BTreeSet<u32>,
    pub(super) source: Option<Handle<WorldAsset>>,
    original_bounds: Option<ExpectedModelBounds>,
    pub(super) preserve_hierarchy: bool,
}

pub(crate) use crate::cluster::flat_source::OriginalStockScene;

#[derive(Clone)]
struct SelectedTemplate {
    scene: Handle<WorldAsset>,
    bounds: ExpectedModelBounds,
    source_bounds: ExpectedModelBounds,
    preserve_hierarchy: bool,
}

impl PartialStock {
    pub(super) fn for_demand(
        package: &Arc<CertifiedPackage>,
        row: &ReferenceRow,
        owned_mode: bool,
        beyond_grid: bool,
        facts: Option<&OwnershipFacts>,
    ) -> Option<Self> {
        (owned_mode && beyond_grid)
            .then(|| Self::candidate_with_facts(package, row, facts))
            .flatten()
    }

    #[cfg(test)]
    fn candidate(package: &Arc<CertifiedPackage>, row: &ReferenceRow) -> Option<Self> {
        Self::candidate_with_facts(package, row, None)
    }

    fn candidate_with_facts(
        package: &Arc<CertifiedPackage>,
        row: &ReferenceRow,
        facts: Option<&OwnershipFacts>,
    ) -> Option<Self> {
        let placed = package.placed_reference(row.form_id)?;
        // A wholly refused reference still uses ordinary full-reference spawning.
        package.reference(row.form_id)?;
        let mut refused = placed.stock_primitives.clone().unwrap_or_default();
        if let Some(facts) = facts.filter(|facts| facts.package_matches(package)) {
            refused.extend(facts.permanent_refused_ordinals(row.form_id));
        }
        if refused.is_empty() {
            return None;
        }
        Some(Self {
            package: package.clone(),
            row: row.clone(),
            refused,
            source: None,
            original_bounds: None,
            preserve_hierarchy: false,
        })
    }
}

#[derive(Resource, Default)]
struct Templates {
    selected: HashMap<(AssetId<WorldAsset>, Vec<u32>), SelectedTemplate>,
}

pub(super) fn install(app: &mut App) {
    super::flat_stock::install(app);
    app.init_resource::<Templates>().add_systems(
        Update,
        source_changes
            .before(super::arm_pending_models)
            .after(super::far_stock::collect),
    );
}

fn scene_bounds(scene: &WorldAsset, meshes: &Assets<Mesh>) -> Result<ExpectedModelBounds, String> {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for entity in scene.world.iter_entities() {
        let Some(mesh) = entity.get::<Mesh3d>() else {
            continue;
        };
        let model = entity
            .get::<crate::cluster::source_identity::SourcePrimitiveTransform>()
            .ok_or("stock source primitive has no model transform")?
            .0;
        let aabb = meshes
            .get(&mesh.0)
            .and_then(Mesh::compute_aabb)
            .ok_or("stock source primitive has no loaded finite mesh bounds")?;
        let center = Vec3::from(aabb.center);
        let extent = Vec3::from(aabb.half_extents);
        let bounds = InstanceBounds::transformed(center - extent, center + extent, model);
        min = min.min(bounds.min);
        max = max.max(bounds.max);
    }
    ExpectedModelBounds::new(min, max)
        .ok_or_else(|| "stock source scene has no finite nonempty bounds".to_owned())
}

fn template(
    world: &mut World,
    source: &Handle<WorldAsset>,
    ordinals: &BTreeSet<u32>,
) -> Result<SelectedTemplate, String> {
    let key = (source.id(), ordinals.iter().copied().collect::<Vec<_>>());
    if let Some(handle) = world.resource::<Templates>().selected.get(&key) {
        return Ok(handle.clone());
    }
    let registry = world.resource::<AppTypeRegistry>().clone();
    let (selected, source_bounds, preserve_hierarchy) = {
        let assets = world.resource::<Assets<WorldAsset>>();
        let source = assets
            .get(source)
            .ok_or("partial stock source scene disappeared")?;
        let source_bounds = scene_bounds(source, world.resource::<Assets<Mesh>>())?;
        let preserve_hierarchy = source.world.iter_entities().any(|entity| {
            entity.contains::<bevy::mesh::skinning::SkinnedMesh>()
                || entity.contains::<bevy::mesh::morph::MorphWeights>()
                || entity.contains::<bevy::mesh::morph::MeshMorphWeights>()
        });
        let selected = if preserve_hierarchy {
            let all: BTreeSet<_> = source
                .world
                .iter_entities()
                .filter(|node| node.contains::<Mesh3d>())
                .map(|node| {
                    node.get::<crate::cluster::source_identity::SourcePrimitive>()
                        .map(|tag| tag.ordinal)
                        .ok_or("partial stock source mesh has no ordinal")
                })
                .collect::<Result<_, _>>()?;
            if !ordinals.is_subset(&all) {
                return Err("partial stock source lost a refused ordinal".to_owned());
            }
            registry
                .write()
                .register::<crate::cluster::ownership::OwnedPrimitiveSource>();
            registry
                .write()
                .register::<crate::cluster::source_identity::SourcePrimitiveMesh>();
            crate::cluster::ownership::clone_with_stock_ordinals(source, ordinals, &registry)?
        } else {
            super::partial_stock_template::selected_scene(source, ordinals, &registry)?
        };
        (selected, source_bounds, preserve_hierarchy)
    };
    // Retained dependency nodes carry nonrender source geometry. The ordinary
    // readiness query validates their complete source bounds without a draw body.
    let bounds = if preserve_hierarchy {
        source_bounds
    } else {
        scene_bounds(&selected, world.resource::<Assets<Mesh>>())?
    };
    let scene = world.resource_mut::<Assets<WorldAsset>>().add(selected);
    let template = SelectedTemplate {
        scene,
        bounds,
        source_bounds,
        preserve_hierarchy,
    };
    world
        .resource_mut::<Templates>()
        .selected
        .insert(key, template.clone());
    Ok(template)
}

fn validate_source_bounds(
    expected: ExpectedModelBounds,
    actual: ExpectedModelBounds,
) -> Result<(), String> {
    ExpectedModelBounds::new(expected.min, expected.max)
        .ok_or("partial stock source bounds are non-finite, empty, or inverted")?;
    // The complete source, not its subset, proves the converter's bounds contract.
    let extent = (expected.max - expected.min).abs().max_element().max(1.0);
    let tolerance = (extent * 1.0e-4).max(1.0e-3);
    let error = (actual.min - expected.min)
        .abs()
        .max((actual.max - expected.max).abs())
        .max_element();
    if !error.is_finite() || error > tolerance {
        return Err("partial stock source bounds diverge from conversion".to_owned());
    }
    Ok(())
}

pub(super) fn arm(world: &mut World, root: Entity, source: Handle<WorldAsset>) {
    let failed = world
        .get_resource::<AssetServer>()
        .and_then(|server| server.get_load_states(source.id()))
        .is_some_and(|(load, _, recursive)| {
            matches!(load, LoadState::Failed(_))
                || matches!(recursive, RecursiveDependencyLoadState::Failed(_))
        });
    if failed {
        world.entity_mut(root).remove::<PartialStock>();
        crate::cluster::ownership::fail_closed(world, root, source);
        return;
    }
    if let Err(reason) = arm_inner(world, root, source.clone()) {
        error!(?root, %reason, "distant refused stock instancing failed; owned bodies remain absent");
        world.entity_mut(root).remove::<PartialStock>();
        crate::cluster::ownership::fail_closed(world, root, source);
        crate::cluster::ownership::refuse_source_instance(
            world,
            root,
            format!("distant refused stock instancing failed: {reason}"),
        );
    }
}

fn arm_inner(world: &mut World, root: Entity, source: Handle<WorldAsset>) -> Result<(), String> {
    let original_bounds = world
        .get::<PartialStock>(root)
        .and_then(|partial| partial.original_bounds)
        .or_else(|| world.get::<ExpectedModelBounds>(root).copied())
        .ok_or("partial stock source has no expected model bounds")?;
    let partial = world
        .get::<PartialStock>(root)
        .ok_or("partial stock root disappeared")?;
    let mut refused = partial.refused.clone();
    if let Some(facts) = world
        .get_resource::<OwnershipFacts>()
        .filter(|facts| facts.package_matches(&partial.package))
    {
        refused.extend(facts.permanent_refused_ordinals(partial.row.form_id));
    }
    let selected = template(world, &source, &refused)?;
    validate_source_bounds(original_bounds, selected.source_bounds)?;
    {
        let mut partial = world
            .get_mut::<PartialStock>(root)
            .ok_or("partial stock root disappeared")?;
        partial.source = Some(source.clone());
        partial.original_bounds = Some(original_bounds);
        partial.refused = refused.clone();
        partial.preserve_hierarchy = selected.preserve_hierarchy;
    }
    world
        .entity_mut(root)
        .insert((
            WorldAssetRoot(selected.scene),
            OriginalStockScene(source),
            selected.bounds,
        ))
        .remove::<(PendingModel, crate::cluster::ownership::OwnershipReference)>();
    if !selected.preserve_hierarchy && world.get::<PendingAssetProfile>(root).is_none() {
        super::flat_stock::queue(world, root);
    }
    Ok(())
}

fn clear_instance(world: &mut World, root: Entity) {
    let flat = world
        .get::<crate::cluster::flat_source::FlatSourcePrimitives>(root)
        .map(|members| members.iter().collect::<Vec<_>>())
        .unwrap_or_default();
    for entity in flat {
        world.despawn(entity);
    }
    world.entity_mut(root).despawn_children();
}

fn pending_profile(row: &ReferenceRow) -> PendingAssetProfile {
    PendingAssetProfile {
        started: std::time::Instant::now(),
        scene_spawned: false,
        path: row
            .model_path
            .clone()
            .and_then(super::converted_model_path)
            .unwrap_or_default(),
        form_id: row.form_id,
        base_form_id: row.base_form_id,
        base_record_type: row.base_record_type.clone(),
        static_physics: false,
        cell_id: row.cell_id,
        actor_scene: false,
    }
}

/// Promotion restores the complete gameplay hierarchy with owned render bodies
/// already removed. There is never a transient complete stock render scene.
pub(super) fn promote(world: &mut World, root: Entity) {
    let Some(partial) = world.entity_mut(root).take::<PartialStock>() else {
        return;
    };
    let Some(source) = partial.source else {
        return;
    };
    clear_instance(world, root);
    if let Some(bounds) = partial.original_bounds {
        if let Some(pose) = world.get::<Transform>(root).copied() {
            world.entity_mut(root).insert(InstanceBounds::transformed(
                bounds.min,
                bounds.max,
                pose.to_matrix(),
            ));
        }
        world.entity_mut(root).insert(bounds);
    }
    let row = partial.row;
    let mut stock = partial.refused;
    if let Some(facts) = world
        .get_resource::<OwnershipFacts>()
        .filter(|facts| facts.package_matches(&partial.package))
    {
        stock.extend(facts.permanent_refused_ordinals(row.form_id));
    }
    world.entity_mut(root).insert((
        pending_profile(&row),
        crate::cluster::ownership::OwnershipReference(row.form_id),
        OriginalStockScene(source.clone()),
    ));
    match crate::cluster::ownership::filtered_scene(world, &source, &stock) {
        Ok(scene) => {
            world.entity_mut(root).insert(WorldAssetRoot(scene));
        }
        Err(reason) => {
            error!(?root, %reason, "promoted owned source refused; no stock body is permitted");
            crate::cluster::ownership::fail_closed(world, root, source);
            crate::cluster::ownership::refuse_source_instance(
                world,
                root,
                format!("promoted owned source refused: {reason}"),
            );
        }
    }
}

fn source_changes(
    mut events: MessageReader<AssetEvent<WorldAsset>>,
    partials: Query<(Entity, &PartialStock)>,
    mut commands: Commands,
) {
    let changed: HashSet<_> = events
        .read()
        .filter_map(|event| match event {
            AssetEvent::Modified { id } | AssetEvent::Removed { id } => Some(*id),
            _ => None,
        })
        .collect();
    if changed.is_empty() {
        return;
    }
    let roots: Vec<_> = partials
        .iter()
        .filter_map(|(root, partial)| {
            partial
                .source
                .as_ref()
                .filter(|source| changed.contains(&source.id()))
                .map(|source| (root, source.clone(), partial.row.clone()))
        })
        .collect();
    commands.queue(move |world: &mut World| {
        world
            .resource_mut::<Templates>()
            .selected
            .retain(|(source, _), _| !changed.contains(source));
        for (root, source, row) in roots {
            clear_instance(world, root);
            world.entity_mut(root).insert(pending_profile(&row));
            // Selection is still the frozen refusal mask after edits or failures.
            arm(world, root, source);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("partial_stock_readiness_tests.rs");
    include!("partial_stock_owned_tests.rs");
    include!("partial_stock_load_failure_tests.rs");
    use crate::{
        cluster::reader::producer_fixture_tests::published_for_inventory,
        world::database::{CellKey, WorldDatabase},
    };
    fn fixture() -> (tempfile::TempDir, Arc<CertifiedPackage>, ReferenceRow) {
        let directory = published_for_inventory();
        let package = Arc::new(
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
        );
        let reference = package
            .records()
            .map(|(_, identity)| identity.reference)
            .find(|id| {
                package.placed_reference(*id).is_some_and(|placed| {
                    placed
                        .stock_primitives
                        .as_ref()
                        .is_some_and(|p| !p.is_empty())
                })
            })
            .unwrap();
        let snapshot = &package.placed_reference(reference).unwrap().snapshot;
        let key = CellKey::Exterior {
            worldspace_id: snapshot.placed_worldspace_id,
            grid_x: snapshot.cell.0,
            grid_y: snapshot.cell.1,
        };
        let database = WorldDatabase::open(&directory.path().join("skyrim_world.db")).unwrap();
        let row = database
            .request_references(1, key, BTreeSet::from([reference]))
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(30))
            .unwrap()
            .result
            .unwrap()
            .pop()
            .unwrap();
        (directory, package, row)
    }

    #[test]
    fn in_grid_and_baked_demands_keep_full_spawning() {
        let (_directory, package, row) = fixture();
        assert!(PartialStock::for_demand(&package, &row, true, true, None).is_some());
        assert!(PartialStock::for_demand(&package, &row, true, false, None).is_none());
        assert!(PartialStock::for_demand(&package, &row, false, true, None).is_none());
    }

    #[test]
    fn initial_material_refusal_creates_partial_even_without_convert_refusals() {
        // The inventory fixture deliberately exports only partial mapped
        // references. Use the complete loader fixture for this contract.
        let directory = crate::cluster::reader::producer_fixture_tests::published_for_loader();
        let package = Arc::new(
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
        );
        let (key, identity) = package.records().next().unwrap();
        let placed = package.placed_reference(identity.reference).unwrap();
        assert_eq!(
            placed.coverage,
            shared::cluster_mapping::CoverageState::Complete
        );
        assert!(placed.stock_primitives.as_ref().unwrap().is_empty());
        let cell = CellKey::Exterior {
            worldspace_id: placed.snapshot.placed_worldspace_id,
            grid_x: placed.snapshot.cell.0,
            grid_y: placed.snapshot.cell.1,
        };
        let database = WorldDatabase::open(&directory.path().join("skyrim_world.db")).unwrap();
        let row = database
            .request_references(1, cell, BTreeSet::from([identity.reference]))
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(30))
            .unwrap()
            .result
            .unwrap()
            .pop()
            .unwrap();
        let mut facts = OwnershipFacts::default();
        facts.publish(&package, std::collections::BTreeMap::from([(*key, true)]));
        assert!(PartialStock::for_demand(&package, &row, true, true, None).is_none());
        let partial = PartialStock::for_demand(&package, &row, true, true, Some(&facts)).unwrap();
        assert_eq!(partial.refused, BTreeSet::from([identity.primitive]));
    }

    #[test]
    fn completely_refused_reference_keeps_full_spawning() {
        let (directory, package, _) = fixture();
        let (grid, ids) = package
            .placed_cells_in_window(0x70, IVec2::ZERO, None)
            .find(|(_, ids)| {
                ids.iter().any(|id| {
                    package
                        .placed_reference(*id)
                        .is_some_and(|p| p.model_path.is_some() && package.reference(*id).is_none())
                })
            })
            .unwrap();
        let reference = *ids
            .iter()
            .find(|id| {
                package
                    .placed_reference(**id)
                    .is_some_and(|p| p.model_path.is_some() && package.reference(**id).is_none())
            })
            .unwrap();
        let database = WorldDatabase::open(&directory.path().join("skyrim_world.db")).unwrap();
        let row = database
            .request_references(
                1,
                CellKey::Exterior {
                    worldspace_id: 0x70,
                    grid_x: grid.x,
                    grid_y: grid.y,
                },
                BTreeSet::from([reference]),
            )
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(30))
            .unwrap()
            .result
            .unwrap()
            .pop()
            .unwrap();
        assert!(PartialStock::for_demand(&package, &row, true, true, None).is_none());
    }

    #[test]
    fn owned_bodies_absent_during_distant_instancing_and_loaded_promotion() {
        use crate::{
            cluster::source_identity::{SourcePrimitive, SourcePrimitiveTransform},
            world::components::WorldTransform,
        };
        use bevy::{
            asset::AssetPlugin,
            camera::visibility::VisibilityClass,
            transform::{TransformPlugin, components::TransformTreeChanged},
            world_serialization::WorldSerializationPlugin,
        };
        let (_directory, package, row) = fixture();
        let refused = package
            .placed_reference(row.form_id)
            .unwrap()
            .stock_primitives
            .clone()
            .unwrap();
        let owned: BTreeSet<_> = package
            .records()
            .filter_map(|(_, identity)| {
                (identity.reference == row.form_id).then_some(identity.primitive)
            })
            .collect();
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin::default(),
            TransformPlugin,
            WorldSerializationPlugin,
        ))
        .init_asset::<Mesh>()
        .register_type::<Name>()
        .register_type::<Transform>()
        .register_type::<GlobalTransform>()
        .register_type::<TransformTreeChanged>()
        .register_type::<ChildOf>()
        .register_type::<Children>()
        .register_type::<Visibility>()
        .register_type::<InheritedVisibility>()
        .register_type::<ViewVisibility>()
        .register_type::<VisibilityClass>()
        .register_type::<Mesh3d>()
        .register_type::<MeshMaterial3d<StandardMaterial>>()
        .register_type::<SourcePrimitive>()
        .register_type::<SourcePrimitiveTransform>()
        .init_resource::<Templates>();
        super::super::flat_stock::install(&mut app);
        let mesh = app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(Cuboid::new(2.0, 2.0, 2.0));
        let material = Handle::<StandardMaterial>::default();
        let mut source = World::new();
        for ordinal in owned.union(&refused) {
            source.spawn((
                Transform::IDENTITY,
                Mesh3d(mesh.clone()),
                MeshMaterial3d(material.clone()),
                SourcePrimitive {
                    scene_index: 0,
                    ordinal: *ordinal,
                    mesh_index: 0,
                    primitive_index: *ordinal,
                    matches_default_scene: true,
                },
                SourcePrimitiveTransform(Mat4::IDENTITY),
            ));
        }
        let source = app
            .world_mut()
            .resource_mut::<Assets<WorldAsset>>()
            .add(WorldAsset::new(source));
        // A post-certification primitive keeps source/gameplay data but cannot
        // acquire stock permission from unchanged placement or aggregate bounds.
        {
            let mut assets = app.world_mut().resource_mut::<Assets<WorldAsset>>();
            let mut scene = assets.get_mut(&source).unwrap();
            scene.world.spawn((
                Transform::IDENTITY,
                Mesh3d(mesh.clone()),
                MeshMaterial3d(material.clone()),
                SourcePrimitive {
                    scene_index: 0,
                    ordinal: u32::MAX,
                    mesh_index: 0,
                    primitive_index: 0,
                    matches_default_scene: true,
                },
                SourcePrimitiveTransform(Mat4::IDENTITY),
            ));
        }
        let pose = Transform::from_xyz(17.0, 23.0, -9.0)
            .with_rotation(Quat::from_rotation_y(0.4))
            .with_scale(Vec3::splat(1.75));
        let root = app
            .world_mut()
            .spawn((
                pose,
                WorldTransform(pose.to_matrix()),
                Visibility::Inherited,
                PartialStock::candidate(&package, &row).unwrap(),
                crate::cluster::ownership::OwnershipReference(row.form_id),
                ExpectedModelBounds::new(Vec3::splat(-1.0), Vec3::ONE).unwrap(),
            ))
            .id();
        arm_inner(app.world_mut(), root, source.clone()).unwrap();
        assert!(
            app.world()
                .get::<crate::cluster::ownership::OwnershipReference>(root)
                .is_none()
        );
        app.update();
        let draws: Vec<_> = app
            .world_mut()
            .query::<(
                &SourcePrimitive,
                &MeshMaterial3d<StandardMaterial>,
                &GlobalTransform,
            )>()
            .iter(app.world())
            .map(|(tag, material, global)| (tag.ordinal, material.0.clone(), global.to_matrix()))
            .collect();
        assert_eq!(draws.len(), refused.len());
        for (ordinal, actual_material, matrix) in draws {
            assert!(refused.contains(&ordinal));
            assert_eq!(actual_material, material);
            assert_eq!(matrix, pose.to_matrix());
        }
        for entity in app
            .world()
            .get::<crate::cluster::flat_source::FlatSourcePrimitives>(root)
            .unwrap()
            .iter()
        {
            assert!(app.world().get::<Transform>(entity).is_none());
            assert!(app.world().get::<ChildOf>(entity).is_none());
        }
        promote(app.world_mut(), root);
        app.update();
        assert!(app.world().get::<PartialStock>(root).is_none());
        assert_eq!(
            app.world()
                .get::<crate::cluster::ownership::OwnershipReference>(root)
                .unwrap()
                .0,
            row.form_id
        );
        assert_ne!(app.world().get::<WorldAssetRoot>(root).unwrap().0, source);
        let expected_count = refused.len();
        assert_eq!(
            app.world_mut().query::<&Mesh3d>().iter(app.world()).count(),
            expected_count
        );
    }

    #[test]
    fn subset_bounds_cannot_mask_invalid_original_conversion_bounds() {
        let bounds = ExpectedModelBounds::new(Vec3::splat(-1.0), Vec3::ONE).unwrap();
        assert!(validate_source_bounds(bounds, bounds).is_ok());
        let wrong = ExpectedModelBounds::new(Vec3::ZERO, Vec3::ONE).unwrap();
        assert!(validate_source_bounds(wrong, bounds).is_err());
    }

    #[test]
    fn missing_original_bounds_fails_before_deriving_subset_assets() {
        let (_directory, package, row) = fixture();
        let mut world = World::new();
        let root = world
            .spawn(PartialStock::candidate(&package, &row).unwrap())
            .id();
        assert_eq!(
            arm_inner(&mut world, root, Handle::default()).unwrap_err(),
            "partial stock source has no expected model bounds"
        );
    }

    #[test]
    fn skin_and_morph_sources_preserve_dependency_graph_without_owned_bodies() {
        use crate::cluster::source_identity::{SourcePrimitive, SourcePrimitiveTransform};
        use bevy::mesh::{
            morph::{MeshMorphWeights, MorphWeights},
            skinning::SkinnedMesh,
        };
        let (_directory, package, row) = fixture();
        let refused = package
            .placed_reference(row.form_id)
            .unwrap()
            .stock_primitives
            .clone()
            .unwrap();
        let owned: BTreeSet<_> = package
            .records()
            .filter_map(|(_, identity)| {
                (identity.reference == row.form_id).then_some(identity.primitive)
            })
            .collect();
        for morph in [false, true] {
            let mut app = super::super::tests::model_app();
            app.register_type::<SkinnedMesh>()
                .register_type::<MorphWeights>()
                .register_type::<MeshMorphWeights>()
                .register_type::<SourcePrimitive>()
                .register_type::<SourcePrimitiveTransform>();
            install(&mut app);
            let mesh = app
                .world_mut()
                .resource_mut::<Assets<Mesh>>()
                .add(Cuboid::new(2.0, 2.0, 2.0));
            let mut scene = World::new();
            let support = scene.spawn(Transform::IDENTITY).id();
            if morph {
                scene
                    .entity_mut(support)
                    .insert(MorphWeights::new(vec![0.5], None).unwrap());
            }
            for &ordinal in owned.union(&refused) {
                let draw = scene
                    .spawn((
                        Mesh3d(mesh.clone()),
                        Transform::IDENTITY,
                        ChildOf(support),
                        SourcePrimitive {
                            scene_index: 0,
                            ordinal,
                            mesh_index: 0,
                            primitive_index: ordinal,
                            matches_default_scene: true,
                        },
                        SourcePrimitiveTransform(Mat4::IDENTITY),
                    ))
                    .id();
                if refused.contains(&ordinal) {
                    if morph {
                        scene
                            .entity_mut(draw)
                            .insert(MeshMorphWeights::Reference(support));
                    } else {
                        scene.entity_mut(draw).insert(SkinnedMesh {
                            inverse_bindposes: Handle::default(),
                            joints: vec![support],
                        });
                    }
                }
            }
            let source = app
                .world_mut()
                .resource_mut::<Assets<WorldAsset>>()
                .add(WorldAsset::new(scene));
            let root = app
                .world_mut()
                .spawn((
                    Transform::IDENTITY,
                    PartialStock::candidate(&package, &row).unwrap(),
                    ExpectedModelBounds::new(Vec3::splat(-1.0), Vec3::ONE).unwrap(),
                ))
                .id();
            arm_inner(app.world_mut(), root, source).unwrap();
            app.update();
            let draws: Vec<_> = app
                .world_mut()
                .query::<(&SourcePrimitive, &Mesh3d)>()
                .iter(app.world())
                .map(|(tag, _)| tag.ordinal)
                .collect();
            assert_eq!(draws.into_iter().collect::<BTreeSet<_>>(), refused);
            if morph {
                for weights in app
                    .world_mut()
                    .query::<&MeshMorphWeights>()
                    .iter(app.world())
                {
                    let MeshMorphWeights::Reference(parent) = weights else {
                        panic!("lost morph reference")
                    };
                    assert_eq!(
                        app.world().get::<MorphWeights>(*parent).unwrap().weights(),
                        &[0.5]
                    );
                }
            } else {
                for skin in app.world_mut().query::<&SkinnedMesh>().iter(app.world()) {
                    assert_eq!(skin.joints.len(), 1);
                    assert!(app.world().get::<Transform>(skin.joints[0]).is_some());
                }
            }
        }
    }

    #[test]
    fn pending_source_edit_keeps_frozen_partial_selection() {
        let (_directory, package, row) = fixture();
        let mut app = App::new();
        app.init_resource::<Templates>()
            .add_message::<AssetEvent<WorldAsset>>()
            .add_systems(Update, source_changes);
        let handle = Handle::<WorldAsset>::default();
        let root = app
            .world_mut()
            .spawn((
                PartialStock::candidate(&package, &row).unwrap(),
                PendingModel {
                    handle: handle.clone(),
                    sequence: 0,
                },
            ))
            .id();
        app.world_mut()
            .write_message(AssetEvent::<WorldAsset>::Modified { id: handle.id() });
        app.update();
        assert!(app.world().get::<PartialStock>(root).is_some());
        assert_eq!(
            app.world().get::<PendingModel>(root).unwrap().handle,
            handle
        );
        assert!(app.world().get::<WorldAssetRoot>(root).is_none());
    }

    #[test]
    fn runtime_fallback_demand_cannot_arm_raw_owned_source() {
        let (_directory, package, row) = fixture();
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            bevy::asset::AssetPlugin::default(),
            bevy::world_serialization::WorldSerializationPlugin,
        ));
        app.init_resource::<crate::cluster::source_fallback::SourceFallbackDemand>();
        app.world_mut()
            .resource_mut::<crate::cluster::source_fallback::SourceFallbackDemand>()
            .update(Some(&package), BTreeSet::from([row.form_id]));
        let root = app
            .world_mut()
            .spawn(PartialStock::candidate(&package, &row).unwrap())
            .id();
        let source = Handle::<WorldAsset>::default();
        arm(app.world_mut(), root, source.clone());
        assert_ne!(app.world().get::<WorldAssetRoot>(root).unwrap().0, source);
        assert_eq!(
            app.world().get::<OriginalStockScene>(root).unwrap().0,
            source
        );
    }

    #[test]
    fn malformed_partial_source_and_promotion_refuse_once_after_suppressing_owned_bodies() {
        use crate::streaming::failure::{LoadingRefusal, refuse_loading};
        for promotion in [false, true] {
            for earlier in [None, Some("earlier loading failure")] {
                let (_directory, package, row) = fixture();
                let mut app = App::new();
                app.init_resource::<Assets<WorldAsset>>()
                    .init_resource::<Assets<Mesh>>()
                    .init_resource::<Templates>();
                let source = app
                    .world_mut()
                    .resource_mut::<Assets<WorldAsset>>()
                    .add(WorldAsset::new(World::new()));
                let mut partial = PartialStock::candidate(&package, &row).unwrap();
                partial.original_bounds =
                    Some(ExpectedModelBounds::new(Vec3::splat(-1.0), Vec3::ONE).unwrap());
                if promotion {
                    partial.source = Some(source.clone());
                }
                let root = app
                    .world_mut()
                    .spawn((
                        partial,
                        ExpectedModelBounds::new(Vec3::splat(-1.0), Vec3::ONE).unwrap(),
                        WorldAssetRoot(source.clone()),
                        pending_profile(&row),
                    ))
                    .id();
                let mut exits = app
                    .world()
                    .resource::<Messages<AppExit>>()
                    .get_cursor_current();
                if let Some(reason) = earlier {
                    refuse_loading(app.world_mut(), reason.into());
                }
                if promotion {
                    promote(app.world_mut(), root);
                } else {
                    arm(app.world_mut(), root, source.clone());
                }
                let filtered = app.world().get::<WorldAssetRoot>(root).unwrap();
                assert_ne!(filtered.0.id(), source.id());
                assert!(
                    app.world()
                        .resource::<Assets<WorldAsset>>()
                        .get(&filtered.0)
                        .unwrap()
                        .world
                        .iter_entities()
                        .all(|node| !node.contains::<Mesh3d>())
                );
                assert!(
                    app.world().get::<AssetFailed>(root).is_some(),
                    "a malformed required partial source must retain an explicit failed-root fact"
                );
                let failure = &app.world().resource::<LoadingRefusal>().0;
                if let Some(reason) = earlier {
                    assert_eq!(failure, reason);
                } else {
                    assert!(!failure.is_empty());
                }
                assert_eq!(
                    exits
                        .read(app.world().resource::<Messages<AppExit>>())
                        .cloned()
                        .collect::<Vec<_>>(),
                    [AppExit::error()]
                );
            }
        }
    }
}
