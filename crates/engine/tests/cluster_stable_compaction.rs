//! Production WGSL numeric proof. Run hardware tests with `cargo nextest run -p engine --test cluster_stable_compaction --run-ignored only`.
//! Readback exists only in this test, never in the renderer.
//!
//! The stage table and the cull uniform come from the renderer's own contract module, so a
//! change to a binding or a field cannot pass here with a stale copy. The old atomic-append
//! shader is frozen in `fixtures/` and run on the GPU, never re-implemented on the CPU.
#[path = "../src/cluster/cull_reuse.rs"]
mod cull_reuse;
#[path = "../src/cluster/gpu_contract.rs"]
mod gpu_contract;
#[path = "../src/cluster/stable_order.rs"]
mod stable_order;

use bevy::{
    math::{Mat4, Vec3},
    tasks::block_on,
};
use bytemuck::Zeroable;
use cull_reuse::{CullInputs, CullReuse};
use gpu_contract::{CullParams, DRAW_INDEXED_INDIRECT_BYTES, SELECTED_CLUSTER_BYTES, STAGES};
use std::{borrow::Cow, sync::mpsc, time::Duration};
use wgpu::{util::DeviceExt, *};

const SHADER: &str = include_str!("../src/cluster/shaders/cluster_cull.wgsl");
/// Frozen atomic-compaction reference shader, retained for the independent numeric oracle.
const LEGACY_SHADER: &str = include_str!("fixtures/cluster_cull_atomic_reference.wgsl");
/// The two legacy entry points with the bindings the old host bound to them
/// (the atomic-compaction reference layout): pass 1 appends survivors, pass 2 appends draws.
const LEGACY_STAGES: [(&str, &[u32]); 2] = [
    ("cull_instances", &[0, 3, 6, 8, 9, 10]),
    ("cull", &[0, 1, 2, 3, 4, 5, 6, 7, 8]),
];
const POISON: u32 = 0xa5a5_a5a5;
const SENTINEL: u32 = 0xfeed_beef;
const NO_GROUP: u32 = 0xffff;

const INDEXED_ARG_WORDS: usize = (DRAW_INDEXED_INDIRECT_BYTES / 4) as usize;
const LEGACY_ARG_WORDS: usize = 4;

/// A selected draw: (instance, mesh, cluster, 0) and all five indexed argument words.
/// The signed base_vertex word is compared by its exact bits (zero for pooled indices).
type Draw = ([u32; 4], [u32; 5]);
/// The common selection semantics of indexed production and non-indexed legacy draws.
type Selection = ([u32; 4], u32);

#[test]
fn shader_has_valid_stable_compaction_stages() {
    let module = naga_oil::compose::Composer::default()
        .make_naga_module(naga_oil::compose::NagaModuleDescriptor {
            source: SHADER,
            file_path: "cluster_cull.wgsl",
            ..Default::default()
        })
        .expect("production cull must validate, including barrier uniformity");
    for (name, _) in STAGES {
        assert!(
            module.entry_points.iter().any(|entry| entry.name == name),
            "missing {name}"
        );
    }
}

#[test]
fn stage_contract_keeps_order_and_gives_instance_cull_the_source_order() {
    assert_eq!(
        STAGES.map(|(name, _)| name),
        [
            "cull_instances",
            "prefix_instances",
            "scatter_instances",
            "cull",
            "prefix_draw_tiles",
            "prefix_draw_groups",
            "scatter_clusters",
            "scatter_large_clusters",
        ]
    );
    assert!(
        STAGES[0].1.contains(&13),
        "cull_instances rejects ids past the planned order through binding 13"
    );
}

#[test]
fn legacy_fixture_is_the_frozen_atomic_append_cull() {
    let module = naga_oil::compose::Composer::default()
        .make_naga_module(naga_oil::compose::NagaModuleDescriptor {
            source: LEGACY_SHADER,
            file_path: "cluster_cull_atomic_reference.wgsl",
            ..Default::default()
        })
        .expect("frozen legacy cull must validate");
    for (name, _) in LEGACY_STAGES {
        assert!(
            module.entry_points.iter().any(|entry| entry.name == name),
            "missing legacy {name}"
        );
    }
    for needle in [
        "atomicAdd(&dispatch[3], 1u)",
        "atomicAdd(&counts[entry.y], 1u)",
        "entry.w != params.epoch",
    ] {
        assert!(
            LEGACY_SHADER.contains(needle),
            "not the atomic shader: {needle}"
        );
    }
    assert!(!LEGACY_SHADER.contains("source_order"));
}

/// One cluster of the LOD scene. `object` / `terrain` say, by hand, whether the cluster must be
/// drawn when its mesh is selected against the object or the terrain threshold. `edge` marks
/// cases whose projected error equals a threshold exactly: they only hold where the projection
/// is a single multiplication (orthographic), so perspective runs leave them out.
#[derive(Clone, Copy)]
struct LodCase {
    group: u32,
    refined: u32,
    x: f32,
    triangles: u32,
    edge: bool,
    object: bool,
    terrain: bool,
}

const fn lod(
    group: u32,
    refined: u32,
    x: f32,
    triangles: u32,
    edge: bool,
    object: bool,
    terrain: bool,
) -> LodCase {
    LodCase {
        group,
        refined,
        x,
        triangles,
        edge,
        object,
        terrain,
    }
}

// Group simplification errors. Both views are built so that projected pixels are 4 * error:
// orthographic camera.w = 4, perspective camera.w = 64 at distance 16. Thresholds: object 8,
// terrain 2. So the projected errors are GA 1, GB 4, GC 12, GD 8 (= object threshold),
// GE 2 (= terrain threshold).
const LOD_ERRORS: [f32; 5] = [0.25, 1.0, 3.0, 2.0, 0.5];
const GA: u32 = 0;
const GB: u32 = 1;
const GC: u32 = 2;
const GD: u32 = 3;
const GE: u32 = 4;
const OBJECT_PIXELS: f32 = 8.0;
const TERRAIN_PIXELS: f32 = 2.0;

// A cluster is drawn when its group is over the threshold (strictly) and it either came from
// source geometry or the group that produced it is at or under the threshold.
const LOD_CASES: [LodCase; 14] = [
    lod(GC, NO_GROUP, 0.0, 3, false, true, true), // source, over both
    lod(GB, NO_GROUP, 0.0, 5, false, false, true), // over terrain only
    lod(GA, NO_GROUP, 0.0, 2, false, false, false), // under both
    lod(GC, GB, 0.0, 4, false, true, false),      // parent path: refined 4 <= 8, but 4 > 2
    lod(GC, GC, 0.0, 6, false, false, false),     // refined group over both: finer level wins
    lod(GC, GA, 0.0, 1, false, true, true),       // refined 1 under both
    lod(GB, GA, 0.0, 7, false, false, true),      // group under object; terrain refined 1 <= 2
    lod(GB, GC, 0.0, 2, false, false, false),     // refined over both
    lod(GC, NO_GROUP, 0.0, 0, false, true, true), // selected zero-triangle cluster
    lod(GC, NO_GROUP, -2000.0, 3, false, false, false), // selected, outside the frustum
    lod(GD, NO_GROUP, 0.0, 2, true, false, true), // error == object threshold: not over
    lod(GC, GD, 0.0, 3, true, true, false),       // refined error == object threshold: at or under
    lod(GE, NO_GROUP, 0.0, 4, true, false, false), // error == terrain threshold: not over
    lod(GC, GE, 0.0, 5, true, true, true),        // refined error == terrain threshold: at or under
];

/// CPU cross-check of the hand-written table against the documented rule (the header of
/// cluster_cull.wgsl). It checks my arithmetic in the table, not the shader.
#[test]
fn lod_table_follows_the_documented_selection_rule() {
    for (index, case) in LOD_CASES.iter().enumerate() {
        let frustum_culled = case.x < -1.0;
        for (threshold, expected, name) in [
            (OBJECT_PIXELS, case.object, "object"),
            (TERRAIN_PIXELS, case.terrain, "terrain"),
        ] {
            let over = |group: u32| LOD_ERRORS[group as usize] * 4.0 > threshold;
            let under = |group: u32| LOD_ERRORS[group as usize] * 4.0 <= threshold;
            let selected = over(case.group) && (case.refined == NO_GROUP || under(case.refined));
            assert_eq!(selected && !frustum_culled, expected, "case {index} {name}");
        }
    }
    let kinds: [fn(&LodCase) -> bool; 4] = [
        |case| case.object && !case.terrain,
        |case| case.terrain && !case.object,
        |case| case.refined != NO_GROUP && case.object && case.terrain,
        |case| case.triangles == 0 && case.object,
    ];
    for (kind, predicate) in kinds.iter().enumerate() {
        assert!(
            LOD_CASES.iter().any(predicate),
            "table lacks case kind {kind}"
        );
    }
}

fn instance_words(world: Mat4, mesh: u32, batch: u32, region_start: u32) -> [u32; 20] {
    let mut words = [0u32; 20];
    words[..16].copy_from_slice(&world.to_cols_array().map(f32::to_bits));
    words[16..].copy_from_slice(&[mesh, batch, region_start, 0]);
    words
}

fn mesh_words(
    cluster_base: u32,
    group_base: u32,
    cluster_count: u32,
    terrain: u32,
    index_base: u32,
    bounds: [f32; 4],
) -> [u32; 16] {
    let [x, y, z, radius] = bounds.map(f32::to_bits);
    [
        // Nonzero vertex bases must not leak into indexed arguments: indices are pooled.
        index_base + 31,
        cluster_base,
        0,
        index_base,
        group_base,
        cluster_count,
        terrain,
        0,
        0,
        0,
        0,
        0,
        x,
        y,
        z,
        radius,
    ]
}

fn cluster_words(
    bounds: [f32; 4],
    triangles: u32,
    group: u32,
    refined: u32,
    triangle_offset: u32,
) -> [u32; 12] {
    let [x, y, z, radius] = bounds.map(f32::to_bits);
    [
        x,
        y,
        z,
        radius,
        0,
        0,
        0,
        triangle_offset,
        3 | (triangles << 8) | (group << 16),
        refined,
        0,
        0,
    ]
}

fn group_words(bounds: [f32; 4], error: f32) -> [u32; 8] {
    let [x, y, z, radius] = bounds.map(f32::to_bits);
    [x, y, z, radius, error.to_bits(), 0, 0, 0]
}

/// Host copy of the shared GPU instance, mesh, cluster and group data of one scene.
struct Scene {
    clusters: Vec<u32>,
    groups: Vec<u32>,
    instances: Vec<u32>,
    meshes: Vec<u32>,
    /// Per instance: cluster count of its mesh and where its scratch region starts.
    counts: Vec<u32>,
    starts: Vec<u32>,
    /// Boundary scene only: one cluster per instance, every cluster inside and drawn.
    full: bool,
}

impl Scene {
    fn empty(full: bool) -> Self {
        Self {
            clusters: Vec::new(),
            groups: Vec::new(),
            instances: Vec::new(),
            meshes: Vec::new(),
            counts: Vec::new(),
            starts: Vec::new(),
            full,
        }
    }

    fn len(&self) -> u32 {
        self.counts.len() as u32
    }

    /// Scratch slots (draw slots) of the first `n` instances.
    fn slots_before(&self, n: u32) -> u32 {
        match n.checked_sub(1) {
            Some(last) => self.starts[last as usize] + self.counts[last as usize],
            None => 0,
        }
    }

    fn batch(&self, id: u32) -> u32 {
        self.instances[id as usize * 20 + 17]
    }

    /// The host plan for one view: the first `ordered` instances in `groups` draw groups.
    fn plan(&self, ordered: u32, groups: usize, group_of: impl Fn(u32) -> u32) -> Plan {
        let mut group_ids = vec![Vec::new(); groups];
        let mut capacities = vec![0u32; groups];
        let mut large = Vec::new();
        for id in 0..ordered {
            let group = group_of(id) as usize;
            group_ids[group].push(id);
            capacities[group] += self.counts[id as usize];
            if self.counts[id as usize] > 64 {
                large.push(id);
            }
        }
        let mut next = 0;
        let bases = capacities
            .iter()
            .map(|&capacity| {
                let base = next;
                next += capacity;
                base
            })
            .collect();
        Plan {
            group_ids,
            bases,
            large,
            ordered,
        }
    }
}

struct Plan {
    group_ids: Vec<Vec<u32>>,
    bases: Vec<u32>,
    large: Vec<u32>,
    ordered: u32,
}

impl Plan {
    /// The immutable source order words, checked against the buffer capacity the renderer sizes.
    fn order(&self, capacity_instances: u32) -> Vec<u32> {
        let order = stable_order::source_order(
            self.group_ids.clone(),
            &self.bases,
            &self.large,
            self.ordered,
        );
        assert!(
            order.len() as u64 * 4
                <= stable_order::order_capacity(capacity_instances, self.bases.len())
        );
        assert_eq!(
            order[2], self.ordered,
            "header word 2 is the ordered watermark"
        );
        order
    }

    /// One batch-table entry per group: enabled, group, argument base.
    fn group_entries(&self) -> Vec<[u32; 4]> {
        self.bases
            .iter()
            .enumerate()
            .map(|(group, &base)| [1, group as u32, base, 0])
            .collect()
    }
}

/// The per-view batch table: one entry per batch, then one bit per instance (the mask tail).
fn batch_table(entries: &[[u32; 4]], n: u32, visible: impl Fn(u32) -> bool) -> Vec<u32> {
    let mut table = vec![0u32; (entries.len() + n.div_ceil(128) as usize) * 4];
    for (index, entry) in entries.iter().enumerate() {
        table[index * 4..index * 4 + 4].copy_from_slice(entry);
    }
    let mask = entries.len() * 4;
    for id in (0..n).filter(|&id| visible(id)) {
        table[mask + (id / 32) as usize] |= 1 << (id % 32);
    }
    table
}

/// Orthographic camera looking at everything with a unit error scale; callers adjust the rest.
fn view_params(n: u32, mask_base: u32, epoch: u32, orthographic: bool) -> CullParams {
    let mut params = CullParams::zeroed();
    params.planes = [[1.0, 0.0, 0.0, 0.0]; 5];
    params.camera = [0.0, 0.0, 0.0, 1.0];
    params.error_pixels = 1.0;
    params.near = 0.1;
    params.instance_count = n;
    params.orthographic = u32::from(orthographic);
    params.instance_mask_base = mask_base;
    params.epoch = epoch;
    params
}

/// Boundary scene: instance `id` has the cluster counts around the 64-cluster scatter split and
/// 256-draw tiles; cluster `(id + c) % 5 == 0` is outside the frustum and `c % 11 == 0` has no
/// triangles. Its batch is `id % batches`.
fn boundary_scene(n: u32, full: bool, batches: u32) -> Scene {
    let mut scene = Scene::empty(full);
    let mut slots = 0u32;
    for id in 0..n {
        let batch = if full { 0 } else { id % batches };
        let count = if full {
            1
        } else {
            [0, 1, 2, 63, 64, 65, 129, 255, 256, 257, 513][id as usize % 11]
        };
        scene
            .instances
            .extend(instance_words(Mat4::IDENTITY, id, batch, slots));
        scene.meshes.extend(mesh_words(
            slots,
            0,
            count,
            0,
            17 + slots * 3,
            [10.0, 0.0, 0.0, 1.0],
        ));
        for cluster in 0..count {
            let inside = full || (id + cluster) % 5 != 0;
            let triangles = u32::from(full || cluster % 11 != 0);
            let x = if inside { 10.0 } else { -10.0 };
            scene.clusters.extend(cluster_words(
                [x, 0.0, 0.0, 0.0],
                triangles,
                0,
                NO_GROUP,
                5 + cluster * 2,
            ));
        }
        scene.counts.push(count);
        scene.starts.push(slots);
        slots += count;
    }
    scene.groups.extend(group_words([0.0; 4], 100.0));
    scene
}

/// Expected stable draws of a boundary scene, in group order, source order, cluster order.
fn boundary_expected(scene: &Scene, plan: &Plan, visible: impl Fn(u32) -> bool) -> Vec<Vec<Draw>> {
    plan.group_ids
        .iter()
        .enumerate()
        .map(|(group, ids)| {
            let mut draws = Vec::new();
            for &id in ids.iter().filter(|&&id| visible(id)) {
                for cluster in 0..scene.counts[id as usize] {
                    if scene.full || (id + cluster) % 5 != 0 {
                        let indices = if scene.full || cluster % 11 != 0 {
                            3
                        } else {
                            0
                        };
                        let slot = plan.bases[group] + draws.len() as u32;
                        draws.push(expected_indexed_draw(
                            scene,
                            [id, id, cluster, 0],
                            indices,
                            slot,
                        ));
                    }
                }
            }
            draws
        })
        .collect()
}

/// LOD scene: four instances share one cluster set. Instance 0 draws the object mesh at
/// distance 16, 1 the same clusters through the terrain mesh, 2 the object mesh at distance
/// 1000, 3 the object mesh with a batch the view disables. Instance id == batch == group.
fn lod_scene(cases: &[LodCase]) -> Scene {
    let count = cases.len() as u32;
    let mut scene = Scene::empty(false);
    for error in LOD_ERRORS {
        scene.groups.extend(group_words([0.0; 4], error));
    }
    for (cluster, case) in cases.iter().enumerate() {
        scene.clusters.extend(cluster_words(
            [case.x, 0.0, 0.0, 1.0],
            case.triangles,
            case.group,
            case.refined,
            7 + cluster as u32 * 11,
        ));
    }
    for terrain in 0..2 {
        scene.meshes.extend(mesh_words(
            0,
            0,
            count,
            terrain,
            23 + terrain * 101,
            [0.0, 0.0, 0.0, 1.0],
        ));
    }
    for (id, (x, mesh)) in [(16.0, 0), (16.0, 1), (1000.0, 0), (16.0, 0)]
        .into_iter()
        .enumerate()
    {
        let id = id as u32;
        let world = Mat4::from_translation(Vec3::new(x, 0.0, 0.0));
        scene
            .instances
            .extend(instance_words(world, mesh, id, id * count));
        scene.counts.push(count);
        scene.starts.push(id * count);
    }
    scene
}

/// Expected indexed arguments from the fixture's pooled element and triangle offsets.
fn expected_indexed_draw(scene: &Scene, tuple: [u32; 4], indices: u32, slot: u32) -> Draw {
    let mesh = tuple[1] as usize * 16;
    let cluster = (scene.meshes[mesh + 1] + tuple[2]) as usize * 12;
    let first_index = scene.meshes[mesh + 3] + scene.clusters[cluster + 7] * 3;
    (tuple, [indices, 1, first_index, 0, slot])
}

/// Expected LOD draws per group. Instance 2 remains a perspective survivor,
/// but all its group errors fall below the threshold at distance 1000.
fn lod_expected(
    scene: &Scene,
    cases: &[LodCase],
    orthographic: bool,
    bases: &[u32],
) -> Vec<Vec<Draw>> {
    let draws = |instance: u32, mesh: u32, terrain: bool| -> Vec<Draw> {
        cases
            .iter()
            .enumerate()
            .filter(|(_, case)| if terrain { case.terrain } else { case.object })
            .enumerate()
            .map(|(rank, (cluster, case))| {
                expected_indexed_draw(
                    scene,
                    [instance, mesh, cluster as u32, 0],
                    case.triangles * 3,
                    bases[instance as usize] + rank as u32,
                )
            })
            .collect()
    };
    vec![
        draws(0, 0, false),
        draws(1, 1, true),
        if orthographic {
            draws(2, 0, false)
        } else {
            Vec::new()
        },
        Vec::new(),
    ]
}

/// Shared GPU data: the same buffers are bound by every view.
struct Shared {
    clusters: Buffer,
    groups: Buffer,
    instances: Buffer,
    meshes: Buffer,
}

/// Per-view buffers: everything a view writes, plus its own inputs.
struct ViewBuffers {
    params: Buffer,
    args: Buffer,
    counts: Buffer,
    visible: Buffer,
    list: Buffer,
    dispatch: Buffer,
    table: Buffer,
    prefixes: Buffer,
    selected: Buffer,
    order: Buffer,
    slots: u32,
    args_words: usize,
    groups: usize,
}

impl ViewBuffers {
    fn buffer<'a>(&'a self, shared: &'a Shared, binding: u32) -> &'a Buffer {
        match binding {
            0 => &self.params,
            1 => &shared.clusters,
            2 => &shared.groups,
            3 => &shared.instances,
            4 => &self.args,
            5 => &self.counts,
            6 => &shared.meshes,
            7 => &self.visible,
            8 => &self.list,
            9 => &self.dispatch,
            10 => &self.table,
            11 => &self.prefixes,
            12 => &self.selected,
            13 => &self.order,
            _ => unreachable!("no binding {binding}"),
        }
    }
}

/// A view's buffers with its bind groups for one pipeline set.
struct Bound {
    view: ViewBuffers,
    binds: Vec<BindGroup>,
}

/// One run of a view: all host-written inputs.
struct Job {
    params: CullParams,
    table: Vec<u32>,
    /// Unused by the legacy shader.
    order: Vec<u32>,
    /// Count mode leaves unused arguments alone; fixed mode clears them first.
    count_mode: bool,
    /// Fill scratch and visible (and in count mode the arguments) with a pattern first,
    /// so stale reads and writes outside the expected slots are visible.
    poison: bool,
}

/// What the GPU left in the readback buffers. `list` is the instance list: four planes of
/// `instance_count` words for the stable shader, one vec4 per survivor for the legacy one.
#[derive(Debug, PartialEq, Eq)]
struct Output {
    counts: Vec<u32>,
    args: Vec<u32>,
    args_words: usize,
    visible: Vec<u32>,
    list: Vec<u32>,
    dispatch: Vec<u32>,
}

impl Output {
    /// The draws of every group, in slot order.
    fn draws(&self, bases: &[u32]) -> Vec<Vec<Draw>> {
        assert_eq!(self.args_words, INDEXED_ARG_WORDS);
        bases
            .iter()
            .zip(&self.counts)
            .map(|(&base, &count)| {
                (base..base + count)
                    .map(|slot| {
                        let at = slot as usize * 4;
                        let tuple: [u32; 4] = self.visible[at..at + 4].try_into().unwrap();
                        let args_at = slot as usize * self.args_words;
                        let args = self.args[args_at..args_at + self.args_words]
                            .try_into()
                            .unwrap();
                        (tuple, args)
                    })
                    .collect()
            })
            .collect()
    }

    /// Compare the legacy shader's selection while checking its complete four-word arguments.
    fn legacy_selections(&self, bases: &[u32]) -> Vec<Vec<Selection>> {
        assert_eq!(self.args_words, LEGACY_ARG_WORDS);
        bases
            .iter()
            .zip(&self.counts)
            .map(|(&base, &count)| {
                (base..base + count)
                    .map(|slot| {
                        let at = slot as usize * LEGACY_ARG_WORDS;
                        let tuple = self.visible[at..at + 4].try_into().unwrap();
                        let args = &self.args[at..at + LEGACY_ARG_WORDS];
                        assert_eq!(&args[1..], &[1, 0, slot], "legacy slot {slot} arguments");
                        (tuple, args[0])
                    })
                    .collect()
            })
            .collect()
    }
}

fn selections(draws: Vec<Vec<Draw>>) -> Vec<Vec<Selection>> {
    draws
        .into_iter()
        .map(|group| {
            group
                .into_iter()
                .map(|(tuple, args)| (tuple, args[0]))
                .collect()
        })
        .collect()
}

fn sorted<T: Ord>(mut draws: Vec<Vec<T>>) -> Vec<Vec<T>> {
    for group in &mut draws {
        group.sort_unstable();
    }
    draws
}

/// Exact stable output: counts, every draw in rank order with its argument, no stray write
/// outside those slots, and the terrain tail record after the last draw slot untouched.
fn assert_exact(
    label: &str,
    out: &Output,
    bases: &[u32],
    expected: &[Vec<Draw>],
    count_mode: bool,
    poisoned: bool,
) {
    assert_eq!(
        out.args_words, INDEXED_ARG_WORDS,
        "{label}: argument stride"
    );
    assert_eq!(
        out.args.len() % INDEXED_ARG_WORDS,
        0,
        "{label}: partial argument"
    );
    let slots = out.args.len() / INDEXED_ARG_WORDS;
    assert_eq!(out.counts.len(), expected.len(), "{label}: group count");
    let mut used = vec![false; slots];
    for (group, draws) in expected.iter().enumerate() {
        assert_eq!(
            out.counts[group] as usize,
            draws.len(),
            "{label} group {group}"
        );
        for (rank, (tuple, args)) in draws.iter().enumerate() {
            let slot = bases[group] as usize + rank;
            assert_eq!(
                &out.visible[slot * 4..slot * 4 + 4],
                tuple,
                "{label} group {group} rank {rank}"
            );
            assert_eq!(
                &out.args[slot * INDEXED_ARG_WORDS..(slot + 1) * INDEXED_ARG_WORDS],
                args,
                "{label} group {group} rank {rank} arguments"
            );
            used[slot] = true;
        }
    }
    for slot in (0..slots).filter(|&slot| !used[slot]) {
        let args = &out.args[slot * INDEXED_ARG_WORDS..(slot + 1) * INDEXED_ARG_WORDS];
        if !count_mode {
            assert_eq!(args, &[0; 5], "{label}: unused argument slot {slot}");
        } else if poisoned {
            assert_eq!(args, &[POISON; 5], "{label}: stray argument at slot {slot}");
        }
        if poisoned {
            assert_eq!(
                &out.visible[slot * 4..slot * 4 + 4],
                &[POISON; 4],
                "{label}: stray visible record at slot {slot}"
            );
        }
    }
    assert_eq!(
        &out.visible[slots * 4..],
        &[SENTINEL; 4],
        "{label}: terrain tail"
    );
}

/// CPU proof of fixture encoding and readback strides, including holes and zero-index draws.
#[test]
fn indexed_readback_keeps_five_argument_words_and_four_visible_words() {
    assert_eq!(DRAW_INDEXED_INDIRECT_BYTES, 20);
    assert_eq!(INDEXED_ARG_WORDS, 5);
    let mut scene = Scene::empty(false);
    scene.meshes.extend(mesh_words(1, 0, 1, 0, 101, [0.0; 4]));
    scene.meshes.extend(mesh_words(3, 0, 2, 0, 1009, [0.0; 4]));
    for offset in [2, 7, 11, 13, 19] {
        scene
            .clusters
            .extend(cluster_words([0.0; 4], 2, 0, NO_GROUP, offset));
    }
    let first = expected_indexed_draw(&scene, [7, 1, 1, 0], 6, 1);
    let second = expected_indexed_draw(&scene, [9, 0, 0, 0], 0, 3);
    assert_eq!(first, ([7, 1, 1, 0], [6, 1, 1066, 0, 1]));
    assert_eq!(second, ([9, 0, 0, 0], [0, 1, 122, 0, 3]));
    let bases = [1, 3];
    let expected = vec![vec![first], vec![second]];
    let slots = 5;
    for count_mode in [false, true] {
        let mut out = Output {
            counts: vec![1, 1],
            args: vec![if count_mode { POISON } else { 0 }; slots * INDEXED_ARG_WORDS],
            args_words: INDEXED_ARG_WORDS,
            visible: vec![POISON; (slots + 1) * 4],
            list: Vec::new(),
            dispatch: Vec::new(),
        };
        for (slot, (tuple, args)) in [(1, first), (3, second)] {
            out.args[slot * INDEXED_ARG_WORDS..(slot + 1) * INDEXED_ARG_WORDS]
                .copy_from_slice(&args);
            out.visible[slot * 4..(slot + 1) * 4].copy_from_slice(&tuple);
        }
        out.visible[slots * 4..].copy_from_slice(&[SENTINEL; 4]);
        assert_eq!(out.draws(&bases), expected);
        assert_exact(
            "CPU indexed layout",
            &out,
            &bases,
            &expected,
            count_mode,
            true,
        );

        let mut legacy = Output {
            counts: out.counts.clone(),
            args: vec![0; slots * LEGACY_ARG_WORDS],
            args_words: LEGACY_ARG_WORDS,
            visible: out.visible.clone(),
            list: Vec::new(),
            dispatch: Vec::new(),
        };
        legacy.args[4..8].copy_from_slice(&[6, 1, 0, 1]);
        legacy.args[12..16].copy_from_slice(&[0, 1, 0, 3]);
        assert_eq!(
            legacy.legacy_selections(&bases),
            selections(expected.clone())
        );
    }
}

/// The surviving instances in source order, the survivor count in the dispatch arguments, and
/// no selected count left on a rejected instance (so an unordered id never leaks a count).
fn assert_survivors(label: &str, out: &Output, n: u32, survivors: &[u32]) {
    let n = n as usize;
    assert_eq!(
        out.dispatch[3] as usize,
        survivors.len(),
        "{label}: survivor count"
    );
    assert_eq!(
        &out.list[3 * n..3 * n + survivors.len()],
        survivors,
        "{label}: survivor ids"
    );
    let mut kept = vec![false; n];
    for &id in survivors {
        kept[id as usize] = true;
    }
    for id in (0..n).filter(|&id| !kept[id]) {
        assert_eq!(
            out.list[id], 0,
            "{label}: rejected instance {id} kept a count"
        );
    }
}

struct Gpu {
    device: Device,
    queue: Queue,
    stable: Vec<ComputePipeline>,
    legacy: Vec<ComputePipeline>,
}

fn pipelines(
    device: &Device,
    module: &ShaderModule,
    stages: &[(&str, &[u32])],
) -> Vec<ComputePipeline> {
    stages
        .iter()
        .map(|&(name, bindings)| {
            // Match production's explicit layouts, including bindings unused by an entry
            // point (automatic layouts omit those and cannot test the host contract).
            let entries: Vec<_> = bindings
                .iter()
                .map(|&binding| BindGroupLayoutEntry {
                    binding,
                    visibility: ShaderStages::COMPUTE,
                    ty: BindingType::Buffer {
                        ty: if binding == 0 {
                            BufferBindingType::Uniform
                        } else {
                            BufferBindingType::Storage {
                                read_only: matches!(binding, 1 | 2 | 3 | 6 | 10 | 13),
                            }
                        },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                })
                .collect();
            let bind_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
                label: Some(name),
                entries: &entries,
            });
            let layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
                label: Some(name),
                bind_group_layouts: &[Some(&bind_layout)],
                immediate_size: 0,
            });
            device.create_compute_pipeline(&ComputePipelineDescriptor {
                label: Some(name),
                layout: Some(&layout),
                module,
                entry_point: Some(name),
                compilation_options: Default::default(),
                cache: None,
            })
        })
        .collect()
}

impl Gpu {
    fn new() -> Self {
        let instance = Instance::new(InstanceDescriptor {
            backends: Backends::VULKAN,
            ..InstanceDescriptor::new_without_display_handle()
        });
        let adapter = block_on(instance.request_adapter(&RequestAdapterOptions::default()))
            .expect("GPU required");
        assert_ne!(
            adapter.get_info().device_type,
            DeviceType::Cpu,
            "software adapter is not GPU proof"
        );
        eprintln!("stable compaction adapter: {:?}", adapter.get_info());
        let (device, queue) =
            block_on(adapter.request_device(&DeviceDescriptor::default())).unwrap();
        let module = |label, source| {
            device.create_shader_module(ShaderModuleDescriptor {
                label: Some(label),
                source: ShaderSource::Wgsl(Cow::Borrowed(source)),
            })
        };
        let stable = pipelines(
            &device,
            &module("production cluster compaction", SHADER),
            &STAGES,
        );
        let legacy = pipelines(
            &device,
            &module("frozen atomic-append cluster cull", LEGACY_SHADER),
            &LEGACY_STAGES,
        );
        Self {
            device,
            queue,
            stable,
            legacy,
        }
    }

    fn words_buffer(&self, words: &[u32], min_words: usize) -> Buffer {
        let mut padded = words.to_vec();
        if padded.len() < min_words {
            padded.resize(min_words, 0);
        }
        self.device.create_buffer_init(&util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&padded),
            usage: BufferUsages::STORAGE | BufferUsages::COPY_DST | BufferUsages::COPY_SRC,
        })
    }

    /// Shared buffers holding the whole scene.
    fn shared(&self, scene: &Scene) -> Shared {
        Shared {
            clusters: self.words_buffer(&scene.clusters, 12),
            groups: self.words_buffer(&scene.groups, 8),
            instances: self.words_buffer(&scene.instances, 20),
            meshes: self.words_buffer(&scene.meshes, 16),
        }
    }

    /// Shared buffers with the scene's capacity and no content yet.
    fn shared_zeroed(&self, scene: &Scene) -> Shared {
        let make = |words: &[u32], min_words: usize| {
            self.device.create_buffer(&BufferDescriptor {
                label: None,
                size: words.len().max(min_words) as u64 * 4,
                usage: BufferUsages::STORAGE | BufferUsages::COPY_DST | BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            })
        };
        Shared {
            clusters: make(&scene.clusters, 12),
            groups: make(&scene.groups, 8),
            instances: make(&scene.instances, 20),
            meshes: make(&scene.meshes, 16),
        }
    }

    /// Append-style upload of the first `n` instances of a boundary scene (one mesh each) and
    /// their clusters, leaving whatever older, larger uploads wrote after them in place.
    fn upload_prefix(&self, shared: &Shared, scene: &Scene, n: u32) {
        let slots = scene.slots_before(n) as usize;
        let n = n as usize;
        for (buffer, words) in [
            (&shared.clusters, &scene.clusters[..slots * 12]),
            (&shared.groups, &scene.groups[..]),
            (&shared.instances, &scene.instances[..n * 20]),
            (&shared.meshes, &scene.meshes[..n * 16]),
        ] {
            if !words.is_empty() {
                self.queue
                    .write_buffer(buffer, 0, bytemuck::cast_slice(words));
            }
        }
    }

    fn view(
        &self,
        slots: u32,
        groups: usize,
        entries: usize,
        instances: u32,
        args_words: usize,
    ) -> ViewBuffers {
        let storage = BufferUsages::STORAGE | BufferUsages::COPY_DST | BufferUsages::COPY_SRC;
        let make = |size: u64, usage| {
            self.device.create_buffer(&BufferDescriptor {
                label: None,
                size: size.max(16),
                usage,
                mapped_at_creation: false,
            })
        };
        ViewBuffers {
            params: make(
                std::mem::size_of::<CullParams>() as u64,
                BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            ),
            args: make(
                u64::from(slots.max(1)) * args_words as u64 * 4,
                storage | BufferUsages::INDIRECT,
            ),
            counts: make(groups as u64 * 4, storage | BufferUsages::INDIRECT),
            // The terrain tail sentinel follows the last draw slot.
            visible: make((u64::from(slots) + 1) * 16, storage),
            list: make(u64::from(instances) * 16, storage),
            dispatch: make(16, storage | BufferUsages::INDIRECT),
            table: make(
                (entries as u64 + u64::from(instances.div_ceil(128))) * 16,
                storage,
            ),
            prefixes: make(stable_order::prefix_capacity(instances, groups), storage),
            selected: make(u64::from(slots) * SELECTED_CLUSTER_BYTES, storage),
            order: make(stable_order::order_capacity(instances, groups), storage),
            slots,
            args_words,
            groups,
        }
    }

    fn bind(
        &self,
        pipelines: &[ComputePipeline],
        stages: &[(&str, &[u32])],
        shared: &Shared,
        view: ViewBuffers,
    ) -> Bound {
        let binds = stages
            .iter()
            .enumerate()
            .map(|(stage, &(_, bindings))| {
                self.device.create_bind_group(&BindGroupDescriptor {
                    label: None,
                    layout: &pipelines[stage].get_bind_group_layout(0),
                    entries: &bindings
                        .iter()
                        .map(|&binding| BindGroupEntry {
                            binding,
                            resource: view.buffer(shared, binding).as_entire_binding(),
                        })
                        .collect::<Vec<_>>(),
                })
            })
            .collect();
        Bound { view, binds }
    }

    fn bind_stable(&self, shared: &Shared, view: ViewBuffers) -> Bound {
        self.bind(&self.stable, &STAGES, shared, view)
    }

    fn bind_legacy(&self, shared: &Shared, view: ViewBuffers) -> Bound {
        self.bind(&self.legacy, &LEGACY_STAGES, shared, view)
    }

    fn upload(&self, bound: &Bound, job: &Job) {
        let view = &bound.view;
        self.queue
            .write_buffer(&view.params, 0, bytemuck::bytes_of(&job.params));
        self.queue
            .write_buffer(&view.table, 0, bytemuck::cast_slice(&job.table));
        if !job.order.is_empty() {
            self.queue
                .write_buffer(&view.order, 0, bytemuck::cast_slice(&job.order));
        }
        if job.poison {
            // Require every consumed scratch value to be produced by this frame,
            // including the first run on freshly allocated (normally zeroed) buffers.
            for buffer in [
                &view.list,
                &view.prefixes,
                &view.selected,
                &view.dispatch,
                &view.counts,
            ] {
                let words = vec![POISON; (buffer.size() / 4) as usize];
                self.queue
                    .write_buffer(buffer, 0, bytemuck::cast_slice(&words));
            }
        }
        if job.poison && view.slots != 0 {
            let poison = vec![POISON; view.slots as usize * 4];
            self.queue
                .write_buffer(&view.visible, 0, bytemuck::cast_slice(&poison));
            if job.count_mode {
                let poison = vec![POISON; view.slots as usize * view.args_words];
                self.queue
                    .write_buffer(&view.args, 0, bytemuck::cast_slice(&poison));
            }
        }
        self.queue.write_buffer(
            &view.visible,
            u64::from(view.slots) * 16,
            bytemuck::cast_slice(&[SENTINEL; 4]),
        );
    }

    fn encode_stable(&self, encoder: &mut CommandEncoder, bound: &Bound, job: &Job, stage: usize) {
        let n = job.params.instance_count;
        let order = &job.order;
        let mut pass = encoder.begin_compute_pass(&ComputePassDescriptor::default());
        pass.set_pipeline(&self.stable[stage]);
        pass.set_bind_group(0, &bound.binds[stage], &[]);
        match stage {
            0 | 2 | 6 => pass.dispatch_workgroups(n.div_ceil(64), 1, 1),
            1 => pass.dispatch_workgroups(1, 1, 1),
            3 => pass.dispatch_workgroups_indirect(&bound.view.dispatch, 0),
            4 => pass.dispatch_workgroups(order[1], 1, 1),
            5 => pass.dispatch_workgroups(order[0], 1, 1),
            // Count of the large-instance list that follows the instance words.
            7 => pass.dispatch_workgroups(order[order[3] as usize], 1, 1),
            _ => unreachable!(),
        }
    }

    /// Runs several views in one submission, stage by stage across the views, so a view's
    /// stages are interleaved with every other view's.
    fn run_stable_views(&self, runs: &[(&Bound, &Job)]) -> Vec<Output> {
        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor::default());
        for &(bound, job) in runs {
            self.upload(bound, job);
            if !job.count_mode {
                encoder.clear_buffer(&bound.view.args, 0, None);
            }
        }
        for stage in 0..STAGES.len() {
            for &(bound, job) in runs {
                self.encode_stable(&mut encoder, bound, job, stage);
            }
        }
        self.collect(encoder, runs)
    }

    fn run_stable(&self, bound: &Bound, job: &Job) -> Output {
        self.run_stable_views(&[(bound, job)]).remove(0)
    }

    /// The old host's frame: dispatch arguments seeded [0, 1, 1, 0], counts always cleared,
    /// arguments cleared (the fixed-count fallback), pass 1 over every instance, pass 2 indirect.
    fn run_legacy(&self, bound: &Bound, job: &Job) -> Output {
        assert!(
            !job.count_mode,
            "the legacy frame here is the fixed-count one"
        );
        let view = &bound.view;
        let tiles = job.params.instance_count.div_ceil(64);
        assert!(tiles <= 65_535, "legacy pass 1 is dispatched in one row");
        self.upload(bound, job);
        self.queue
            .write_buffer(&view.dispatch, 0, bytemuck::cast_slice(&[0u32, 1, 1, 0]));
        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor::default());
        encoder.clear_buffer(&view.args, 0, None);
        encoder.clear_buffer(&view.counts, 0, None);
        {
            let mut pass = encoder.begin_compute_pass(&ComputePassDescriptor::default());
            pass.set_pipeline(&self.legacy[0]);
            pass.set_bind_group(0, &bound.binds[0], &[]);
            pass.dispatch_workgroups(tiles, 1, 1);
        }
        {
            let mut pass = encoder.begin_compute_pass(&ComputePassDescriptor::default());
            pass.set_pipeline(&self.legacy[1]);
            pass.set_bind_group(0, &bound.binds[1], &[]);
            pass.dispatch_workgroups_indirect(&view.dispatch, 0);
        }
        self.collect(encoder, &[(bound, job)]).remove(0)
    }

    fn collect(&self, encoder: CommandEncoder, runs: &[(&Bound, &Job)]) -> Vec<Output> {
        let mut parts = Vec::new();
        let mut lens = Vec::new();
        for &(bound, job) in runs {
            let view = &bound.view;
            let slots = view.slots as usize;
            let n = job.params.instance_count as usize;
            let words = [
                view.groups,
                slots * view.args_words,
                (slots + 1) * 4,
                n * 4,
                4,
            ];
            parts.extend([
                (&view.counts, words[0] as u64 * 4),
                (&view.args, words[1] as u64 * 4),
                (&view.visible, words[2] as u64 * 4),
                (&view.list, words[3] as u64 * 4),
                (&view.dispatch, words[4] as u64 * 4),
            ]);
            lens.push((words, view.args_words));
        }
        let words = self.read_words(encoder, &parts);
        let mut at = 0;
        let mut outputs = Vec::new();
        for (lens, args_words) in lens {
            let mut take = lens.map(|len| {
                let part = words[at..at + len].to_vec();
                at += len;
                part
            });
            let dispatch = std::mem::take(&mut take[4]);
            let list = std::mem::take(&mut take[3]);
            let visible = std::mem::take(&mut take[2]);
            let args = std::mem::take(&mut take[1]);
            let counts = std::mem::take(&mut take[0]);
            outputs.push(Output {
                counts,
                args,
                args_words,
                visible,
                list,
                dispatch,
            });
        }
        outputs
    }

    fn read_buffer(&self, buffer: &Buffer, words: usize) -> Vec<u32> {
        let encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor::default());
        self.read_words(encoder, &[(buffer, words as u64 * 4)])
    }

    /// Copies the leading bytes of each buffer, back to back, into one readback buffer.
    fn read_words(&self, mut encoder: CommandEncoder, parts: &[(&Buffer, u64)]) -> Vec<u32> {
        let total: u64 = parts.iter().map(|part| part.1).sum();
        let readback = self.device.create_buffer(&BufferDescriptor {
            label: None,
            size: total.max(16),
            usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut offset = 0;
        for &(buffer, len) in parts {
            if len != 0 {
                encoder.copy_buffer_to_buffer(buffer, 0, &readback, offset, len);
            }
            offset += len;
        }
        self.queue.submit([encoder.finish()]);
        let (send, recv) = mpsc::channel();
        readback.slice(..).map_async(MapMode::Read, move |result| {
            send.send(result).unwrap();
        });
        self.device
            .poll(PollType::Wait {
                submission_index: None,
                timeout: Some(Duration::from_secs(30)),
            })
            .unwrap();
        recv.recv_timeout(Duration::from_secs(30)).unwrap().unwrap();
        let mapped = readback.slice(..).get_mapped_range();
        let words: &[u32] = bytemuck::cast_slice(&mapped);
        let mut words = words.to_vec();
        drop(mapped);
        readback.unmap();
        words.truncate((total / 4) as usize);
        words
    }

    /// Boundary / full scenes against the CPU model, repeatedly on reused buffers: visibility
    /// shrinks and grows, count and fixed-count modes alternate, and a sentinel behind the last
    /// draw slot (the terrain tail) must survive. Stale counts, arguments or instance data must
    /// not leak across epochs.
    fn fixture(&self, n: u32, full: bool) {
        eprintln!("stable compaction fixture: instances={n}, full={full}");
        let groups = 4usize; // Includes a deliberately empty group.
        let scene = boundary_scene(n, full, 3);
        let plan = scene.plan(n, groups, |id| scene.batch(id));
        let order = plan.order(n);
        let shared = self.shared(&scene);
        let bound = self.bind_stable(
            &shared,
            self.view(scene.slots_before(n), groups, groups, n, INDEXED_ARG_WORDS),
        );
        for epoch in 1..=6u32 {
            let enabled = |id: u32| full || (epoch != 3 && !(id + epoch % 2).is_multiple_of(7));
            let job = Job {
                params: view_params(n, groups as u32, epoch, true),
                table: batch_table(&plan.group_entries(), n, enabled),
                order: order.clone(),
                count_mode: epoch.is_multiple_of(2),
                poison: false,
            };
            let out = self.run_stable(&bound, &job);
            let label = format!("n={n} full={full} epoch {epoch}");
            let expected = boundary_expected(&scene, &plan, enabled);
            assert_exact(&label, &out, &plan.bases, &expected, job.count_mode, false);
            let survivors: Vec<u32> = (0..n).filter(|&id| enabled(id)).collect();
            assert_survivors(&label, &out, n, &survivors);
        }
    }
}

#[test]
#[ignore = "hardware GPU proof: cargo nextest run -p engine --test cluster_stable_compaction --run-ignored only"]
fn stable_compaction_matches_cpu_repeatedly() {
    let gpu = Gpu::new();
    for (count, full) in [
        (0, false),
        (1, false),
        (63, false),
        (64, false),
        (65, false),
        (1301, false),
        (66_049, true),
    ] {
        gpu.fixture(count, full);
    }
}

/// The order is planned in Queue; PrepareResources then appends blocks, so the GPU instance
/// count can exceed what the order covers (the per-view mask may even enable the new ids).
/// The appended small and large instances must draw nothing and keep no count until a later
/// plan includes them, while buffers are reused dirty as the instance count grows and shrinks.
#[test]
#[ignore = "hardware GPU proof: cargo nextest run -p engine --test cluster_stable_compaction --run-ignored only"]
fn stable_order_ignores_instances_appended_after_planning() {
    let gpu = Gpu::new();
    let capacity = 600;
    let groups = 4;
    let scene = boundary_scene(capacity, false, 3);
    let shared = gpu.shared_zeroed(&scene);
    let bound = gpu.bind_stable(
        &shared,
        gpu.view(
            scene.slots_before(capacity),
            groups,
            groups,
            capacity,
            INDEXED_ARG_WORDS,
        ),
    );
    // (instances the order was planned for, instances the GPU has).
    let steps = [
        (0, 64),
        (64, 64),
        (64, 200),
        (200, 200),
        (130, 517),
        (130, 600),
        (600, 600),
        (257, 600),
        (100, 100),
    ];
    for (step, &(ordered, total)) in steps.iter().enumerate() {
        let appended = ordered..total;
        if total > ordered {
            assert!(
                appended.clone().any(|id| scene.counts[id as usize] > 64),
                "step {step} must append a large instance"
            );
            assert!(
                appended
                    .clone()
                    .any(|id| (1..=64).contains(&scene.counts[id as usize])),
                "step {step} must append a small instance"
            );
        }
        gpu.upload_prefix(&shared, &scene, total);
        let plan = scene.plan(ordered, groups, |id| scene.batch(id));
        // The mask enables every appended id: only the order's watermark may keep them out.
        let enabled = |id: u32| id >= ordered || !id.is_multiple_of(7);
        let ordered_visible = |id: u32| !id.is_multiple_of(7);
        let job = Job {
            params: view_params(total, groups as u32, step as u32 + 1, true),
            table: batch_table(&plan.group_entries(), total, enabled),
            order: plan.order(capacity),
            count_mode: step % 2 == 1,
            poison: true,
        };
        let out = gpu.run_stable(&bound, &job);
        let label = format!("step {step}: ordered {ordered} of {total}");
        let expected = boundary_expected(&scene, &plan, ordered_visible);
        assert_exact(&label, &out, &plan.bases, &expected, job.count_mode, true);
        let survivors: Vec<u32> = (0..ordered).filter(|&id| ordered_visible(id)).collect();
        assert_survivors(&label, &out, total, &survivors);
    }
}

/// Runs the LOD scene through the stable shader (both argument modes, exact) and, when asked,
/// through the frozen legacy shader, which must pick the same draws in any order.
fn lod_view(gpu: &Gpu, orthographic: bool, with_legacy: bool) {
    // Edge cases have projected error == threshold, exact only for one multiplication.
    let cases: Vec<LodCase> = LOD_CASES
        .into_iter()
        .filter(|case| orthographic || !case.edge)
        .collect();
    let scene = lod_scene(&cases);
    let n = scene.len();
    let plan = scene.plan(n, 4, |id| id);
    let shared = gpu.shared(&scene);
    let slots = scene.slots_before(n);
    let stable = gpu.bind_stable(&shared, gpu.view(slots, 4, 4, n, INDEXED_ARG_WORDS));
    let legacy =
        with_legacy.then(|| gpu.bind_legacy(&shared, gpu.view(slots, 4, 4, n, LEGACY_ARG_WORDS)));
    // Batch 3 is not ready in this view.
    let entries: Vec<[u32; 4]> = plan
        .group_entries()
        .into_iter()
        .map(|entry| [u32::from(entry[1] != 3), entry[1], entry[2], 0])
        .collect();
    let mut params = view_params(n, 4, 1, orthographic);
    params.camera = [0.0, 0.0, 0.0, if orthographic { 4.0 } else { 64.0 }];
    params.error_pixels = OBJECT_PIXELS;
    params.terrain_error_pixels = TERRAIN_PIXELS;
    let expected = lod_expected(&scene, &cases, orthographic, &plan.bases);
    let survivors: &[u32] = &[0, 1, 2];
    let name = if orthographic {
        "orthographic"
    } else {
        "perspective"
    };
    for count_mode in [false, true] {
        params.epoch += 1;
        let job = Job {
            params,
            table: batch_table(&entries, n, |_| true),
            order: plan.order(n),
            count_mode,
            poison: true,
        };
        let label = format!("lod {name} count_mode={count_mode}");
        let out = gpu.run_stable(&stable, &job);
        assert_exact(&label, &out, &plan.bases, &expected, count_mode, true);
        assert_survivors(&label, &out, n, survivors);
        if let (Some(legacy), false) = (&legacy, count_mode) {
            let old = gpu.run_legacy(legacy, &job);
            let label = format!("lod {name} legacy");
            assert_eq!(old.counts, out.counts, "{label}: counts");
            assert_eq!(
                sorted(old.legacy_selections(&plan.bases)),
                sorted(selections(expected.clone())),
                "{label}: draws"
            );
            assert_eq!(
                old.dispatch[3] as usize,
                survivors.len(),
                "{label}: survivors"
            );
        }
    }
}

#[test]
#[ignore = "hardware GPU proof: cargo nextest run -p engine --test cluster_stable_compaction --run-ignored only"]
fn stable_lod_selection_follows_projected_error_rules() {
    let gpu = Gpu::new();
    lod_view(&gpu, true, false);
    lod_view(&gpu, false, false);
}

#[test]
#[ignore = "hardware GPU proof: cargo nextest run -p engine --test cluster_stable_compaction --run-ignored only"]
fn stable_visible_set_matches_frozen_atomic_cull() {
    let gpu = Gpu::new();
    lod_view(&gpu, true, true);
    lod_view(&gpu, false, true);

    // A fixed boundary scene: cluster counts around every scatter and tile split, frustum
    // misses, zero-triangle clusters, a per-view instance mask and an empty group.
    let n = 1301;
    let groups = 4;
    let scene = boundary_scene(n, false, 3);
    let plan = scene.plan(n, groups, |id| scene.batch(id));
    let shared = gpu.shared(&scene);
    let slots = scene.slots_before(n);
    let stable = gpu.bind_stable(
        &shared,
        gpu.view(slots, groups, groups, n, INDEXED_ARG_WORDS),
    );
    let legacy = gpu.bind_legacy(
        &shared,
        gpu.view(slots, groups, groups, n, LEGACY_ARG_WORDS),
    );
    let visible = |id: u32| id % 7 != 3;
    let job = Job {
        params: view_params(n, groups as u32, 1, true),
        table: batch_table(&plan.group_entries(), n, visible),
        order: plan.order(n),
        count_mode: false,
        poison: true,
    };
    let out = gpu.run_stable(&stable, &job);
    let old = gpu.run_legacy(&legacy, &job);
    let expected = boundary_expected(&scene, &plan, visible);
    let survivors: Vec<u32> = (0..n).filter(|&id| visible(id)).collect();
    assert_exact("boundary stable", &out, &plan.bases, &expected, false, true);
    assert_survivors("boundary stable", &out, n, &survivors);
    assert_eq!(old.counts, out.counts, "boundary: counts");
    // The old shader lists survivors in atomic order; the set must match.
    assert_eq!(
        old.dispatch[3] as usize,
        survivors.len(),
        "boundary: survivors"
    );
    let mut old_ids: Vec<u32> = (0..survivors.len())
        .map(|rank| old.list[rank * 4])
        .collect();
    old_ids.sort_unstable();
    assert_eq!(old_ids, survivors, "boundary: legacy survivor set");
    let (old_draws, stable_draws) = (
        sorted(old.legacy_selections(&plan.bases)),
        sorted(selections(out.draws(&plan.bases))),
    );
    for (group, (old_group, stable_group)) in old_draws.iter().zip(&stable_draws).enumerate() {
        assert!(
            old_group == stable_group,
            "boundary group {group}: legacy {} draws vs stable {}",
            old_group.len(),
            stable_group.len()
        );
    }
}

/// Three views read the SAME instance, mesh, cluster and group buffers with different batch to
/// group assignments, tables, masks and cull parameters, each with its own scratch, argument,
/// visible, order and instance-list buffers. Their stages are interleaved in one submission,
/// then each view runs alone again: results must not depend on the other views.
#[test]
#[ignore = "hardware GPU proof: cargo nextest run -p engine --test cluster_stable_compaction --run-ignored only"]
fn stable_views_share_instances_with_independent_scratch() {
    struct Spec {
        /// Per batch: the view's draw group and whether the batch may draw.
        mapping: [(u32, bool); 5],
        groups: usize,
        mask: fn(u32) -> bool,
        orthographic: bool,
        error_pixels: f32,
        count_mode: bool,
    }
    struct Prepared {
        bound: Bound,
        job: Job,
        bases: Vec<u32>,
        expected: Vec<Vec<Draw>>,
        survivors: Vec<u32>,
    }
    let specs = [
        // Four groups, one holding two batches; fixed-count.
        Spec {
            mapping: [(0, true), (1, true), (2, true), (0, true), (3, true)],
            groups: 4,
            mask: |id| !id.is_multiple_of(7),
            orthographic: true,
            error_pixels: 1.0,
            count_mode: false,
        },
        // Three groups, a disabled batch whose instances still sit in a group's list;
        // perspective camera; count mode.
        Spec {
            mapping: [(2, true), (0, true), (0, true), (1, true), (2, false)],
            groups: 3,
            mask: |id| id % 3 != 1,
            orthographic: false,
            error_pixels: 1.0,
            count_mode: true,
        },
        // Everything in one group with three empty groups; the threshold selects nothing.
        Spec {
            mapping: [(0, true); 5],
            groups: 4,
            mask: |_| true,
            orthographic: true,
            error_pixels: 1.0e6,
            count_mode: false,
        },
    ];
    let gpu = Gpu::new();
    let n = 700;
    let scene = boundary_scene(n, false, 5);
    let shared = gpu.shared(&scene);
    let slots = scene.slots_before(n);
    let prepared: Vec<Prepared> = specs
        .iter()
        .enumerate()
        .map(|(index, spec)| {
            let plan = scene.plan(n, spec.groups, |id| {
                spec.mapping[scene.batch(id) as usize].0
            });
            let entries: Vec<[u32; 4]> = spec
                .mapping
                .iter()
                .map(|&(group, enabled)| [u32::from(enabled), group, plan.bases[group as usize], 0])
                .collect();
            let drawn = |id: u32| (spec.mask)(id) && spec.mapping[scene.batch(id) as usize].1;
            let mut params = view_params(
                n,
                entries.len() as u32,
                10 + index as u32,
                spec.orthographic,
            );
            if !spec.orthographic {
                params.camera = [0.0, 0.0, 10.0, 1.0];
            }
            params.error_pixels = spec.error_pixels;
            let expected = if spec.error_pixels > 100.0 {
                vec![Vec::new(); spec.groups]
            } else {
                boundary_expected(&scene, &plan, drawn)
            };
            Prepared {
                bound: gpu.bind_stable(
                    &shared,
                    gpu.view(slots, spec.groups, entries.len(), n, INDEXED_ARG_WORDS),
                ),
                job: Job {
                    params,
                    table: batch_table(&entries, n, drawn),
                    order: plan.order(n),
                    count_mode: spec.count_mode,
                    poison: true,
                },
                bases: plan.bases,
                expected,
                survivors: (0..n).filter(|&id| drawn(id)).collect(),
            }
        })
        .collect();
    let check = |label: &str, view: &Prepared, out: &Output| {
        assert_exact(
            label,
            out,
            &view.bases,
            &view.expected,
            view.job.count_mode,
            true,
        );
        assert_survivors(label, out, n, &view.survivors);
    };
    let runs: Vec<(&Bound, &Job)> = prepared
        .iter()
        .map(|view| (&view.bound, &view.job))
        .collect();
    for (index, (view, out)) in prepared.iter().zip(gpu.run_stable_views(&runs)).enumerate() {
        check(&format!("view {index} interleaved"), view, &out);
    }
    for (index, view) in prepared.iter().enumerate().rev() {
        let out = gpu.run_stable(&view.bound, &view.job);
        check(&format!("view {index} alone"), view, &out);
    }
    // Reading is all any view does to the shared data.
    for (buffer, words, name) in [
        (&shared.instances, &scene.instances, "instances"),
        (&shared.meshes, &scene.meshes, "meshes"),
        (&shared.clusters, &scene.clusters, "clusters"),
        (&shared.groups, &scene.groups, "groups"),
    ] {
        assert!(
            gpu.read_buffer(buffer, words.len()) == *words,
            "shared {name} buffer was modified by a view"
        );
    }
}

/// The production gate belongs to exactly these output allocations. The forced reference
/// owns different allocations but receives the same uploads and dirty-buffer history.
struct ReusedView {
    bound: Bound,
    reuse: CullReuse,
    uniform: Option<CullParams>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct EncodedWork {
    /// Host dispatch calls at the real production-shader encoding sites, not gate decisions.
    stages: [u32; STAGES.len()],
    clears: u32,
}

impl Gpu {
    fn reuse_view(&self, shared: &Shared, scene: &Scene, groups: usize) -> ReusedView {
        ReusedView {
            bound: self.bind_stable(
                shared,
                self.view(
                    scene.slots_before(scene.len()),
                    groups,
                    groups,
                    scene.len(),
                    INDEXED_ARG_WORDS,
                ),
            ),
            reuse: CullReuse::default(),
            uniform: None,
        }
    }

    /// Interleave views exactly as the existing hardware harness does. A retained view
    /// receives no upload, clear, or dispatch; only test-owned readback copies execute.
    fn run_reused_views(
        &self,
        runs: &mut [(&mut ReusedView, &Job, CullInputs)],
        force: bool,
    ) -> (Vec<Output>, Vec<EncodedWork>) {
        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor::default());
        let mut work: Vec<EncodedWork> = runs.iter().map(|_| EncodedWork::default()).collect();
        let recompute: Vec<bool> = runs
            .iter_mut()
            .enumerate()
            .map(|(index, (view, job, inputs))| {
                let uniform_changed = view.uniform.as_ref().is_none_or(|previous| {
                    bytemuck::bytes_of(previous) != bytemuck::bytes_of(&job.params)
                });
                let needed = view.reuse.needs_recompute(*inputs, uniform_changed);
                if needed || force {
                    self.upload(&view.bound, job);
                    view.uniform = Some(job.params);
                    if !job.count_mode {
                        encoder.clear_buffer(&view.bound.view.args, 0, None);
                        work[index].clears += 1;
                    }
                    encoder.clear_buffer(&view.bound.view.counts, 0, None);
                    work[index].clears += 1;
                }
                needed || force
            })
            .collect();
        for stage in 0..STAGES.len() {
            for (index, (view, job, _)) in runs.iter().enumerate() {
                if recompute[index] {
                    self.encode_stable(&mut encoder, &view.bound, job, stage);
                    work[index].stages[stage] += 1;
                }
            }
        }
        for (index, (view, _, inputs)) in runs.iter_mut().enumerate() {
            if recompute[index] {
                view.reuse.encoded(*inputs);
            }
        }
        let reads: Vec<_> = runs
            .iter()
            .map(|(view, job, _)| (&view.bound, *job))
            .collect();
        let mut outputs = self.collect(encoder, &reads);
        // Include the allocation tail when instance_count is below reserved capacity.
        for (output, (view, _, _)) in outputs.iter_mut().zip(runs.iter()) {
            let words = (view.bound.view.list.size() / 4) as usize;
            if output.list.len() != words {
                output.list = self.read_buffer(&view.bound.view.list, words);
            }
        }
        (outputs, work)
    }
}

/// Check complete allocation bytes, including inactive count-mode arguments, unused visible
/// records, instance-list tails, and the terrain sentinel. Comparing only live draws would
/// miss a clear performed on the reuse path or stale indirect dispatch arguments.
fn assert_reuse_output(label: &str, retained: &Output, forced: &Output) {
    assert_eq!(retained.counts, forced.counts, "{label}: counts");
    assert_eq!(retained.args_words, forced.args_words, "{label}: stride");
    assert_eq!(retained.args, forced.args, "{label}: all arguments");
    assert_eq!(
        retained.visible, forced.visible,
        "{label}: all visible records"
    );
    assert_eq!(retained.list, forced.list, "{label}: full instance list");
    assert_eq!(
        retained.dispatch, forced.dispatch,
        "{label}: indirect dispatch"
    );
}

fn assert_reuse_work(label: &str, work: &EncodedWork, job: &Job, recompute: bool) {
    for (stage, &(name, _)) in STAGES.iter().enumerate() {
        assert_eq!(work.stages[stage], u32::from(recompute), "{label}: {name}");
    }
    let clears = if recompute {
        1 + u32::from(!job.count_mode)
    } else {
        0
    };
    assert_eq!(work.clears, clears, "{label}: output clears");
}

/// Each mutation is checked in its first frame, then followed by unchanged frames. Both
/// allocation histories remain dirty after initialization; the reference always runs WGSL.
/// Revision-only cases intentionally preserve shader inputs while requiring fresh dispatch.
#[test]
#[ignore = "hardware GPU proof: cargo nextest run -p engine --test cluster_stable_compaction --run-ignored only"]
fn stable_cull_reuse_matches_forced_recompute_after_mutations() {
    let gpu = Gpu::new();
    for count_mode in [false, true] {
        let capacity = 66;
        let groups = 4;
        let mut scene = boundary_scene(capacity, false, 3);
        // Exercise both object and terrain LOD thresholds in the same view.
        for mesh in scene
            .meshes
            .as_chunks_mut::<16>()
            .0
            .iter_mut()
            .skip(1)
            .step_by(2)
        {
            mesh[6] = 1;
        }
        let shared = gpu.shared_zeroed(&scene);
        gpu.upload_prefix(&shared, &scene, capacity - 1);
        let mut retained = gpu.reuse_view(&shared, &scene, groups);
        let mut forced = gpu.reuse_view(&shared, &scene, groups);
        let mut plan = scene.plan(capacity - 1, groups, |id| scene.batch(id));
        let mut mapping = [0u32, 1, 2, 3];
        let mut entries = plan.group_entries();
        let mut hidden_instance = None;
        let mut job = Job {
            params: view_params(capacity - 1, groups as u32, 1, true),
            table: batch_table(&plan.group_entries(), capacity, |_| true),
            order: plan.order(capacity),
            count_mode,
            poison: true,
        };
        job.params.terrain_error_pixels = 1.0;
        let mut inputs = CullInputs::default();
        let mut previous: Option<Output> = None;
        for change in [
            "initial",
            "view",
            "restore view",
            "frustum",
            "restore frustum",
            "object LOD",
            "terrain LOD",
            "restore LOD",
            "source order",
            "instance transform",
            "instance mask",
            "append count before planning",
            "plan appended instance",
            "gpu revision",
            "layout revision",
            "materials revision",
            "selection revision",
            "plan revision",
            "readiness revision",
            "pipelines revision",
            "origin x",
            "origin y",
            "origin z",
            "allocation replacement",
            "view recreation",
        ] {
            match change {
                "initial" => {}
                "view" => {
                    job.params.orthographic = 0;
                    job.params.camera = [10_000.0, 0.0, 0.0, 1.0];
                }
                "restore view" => {
                    job.params.orthographic = 1;
                    job.params.camera = [0.0, 0.0, 0.0, 1.0];
                }
                "frustum" => job.params.planes[0][3] = -20.0,
                "restore frustum" => job.params.planes[0][3] = 0.0,
                "object LOD" => job.params.error_pixels = 200.0,
                "terrain LOD" => job.params.terrain_error_pixels = 200.0,
                "restore LOD" => {
                    job.params.error_pixels = 1.0;
                    job.params.terrain_error_pixels = 1.0;
                }
                "source order" | "plan appended instance" => {
                    if change == "source order" {
                        // Production sorts IDs inside each group. Change actual group
                        // membership so rebuilding the source order changes its words.
                        mapping[..3].rotate_left(1);
                    }
                    plan = scene.plan(job.params.instance_count, groups, |id| {
                        mapping[scene.batch(id) as usize]
                    });
                    entries = mapping
                        .iter()
                        .map(|&group| [1, group, plan.bases[group as usize], 0])
                        .collect();
                    job.order = plan.order(capacity);
                    job.table = batch_table(&entries, capacity, |id| Some(id) != hidden_instance);
                    inputs.plan += 1;
                }
                "instance transform" => {
                    let id = 3usize; // Small-instance scatter path; later masks hit a large one.
                    let transform = Mat4::from_translation(Vec3::new(-100.0, 0.0, 0.0));
                    scene.instances[id * 20..id * 20 + 16]
                        .copy_from_slice(&transform.to_cols_array().map(f32::to_bits));
                    gpu.queue.write_buffer(
                        &shared.instances,
                        (id * 20 * 4) as u64,
                        bytemuck::cast_slice(&scene.instances[id * 20..(id + 1) * 20]),
                    );
                    inputs.gpu += 1;
                }
                "instance mask" => {
                    hidden_instance = Some(5);
                    job.table = batch_table(&entries, capacity, |id| Some(id) != hidden_instance);
                    inputs.selection += 1;
                }
                "append count before planning" => {
                    gpu.upload_prefix(&shared, &scene, capacity);
                    job.params.instance_count = capacity;
                    inputs.gpu += 1;
                }
                "gpu revision" => inputs.gpu += 1,
                "layout revision" => inputs.layout += 1,
                "materials revision" => inputs.materials += 1,
                "selection revision" => inputs.selection += 1,
                "plan revision" => inputs.plan += 1,
                "readiness revision" => inputs.readiness += 1,
                "pipelines revision" => inputs.pipelines += 1,
                "origin x" => inputs.origin[0] = 1.0f32.to_bits(),
                "origin y" => inputs.origin[1] = 2.0f32.to_bits(),
                "origin z" => inputs.origin[2] = 3.0f32.to_bits(),
                "allocation replacement" | "view recreation" => {
                    retained = gpu.reuse_view(&shared, &scene, groups);
                    forced = gpu.reuse_view(&shared, &scene, groups);
                    // The bytes can equal the previous view's uniform. Fresh allocation
                    // validity must still force encoding independently of that equality.
                    retained.uniform = Some(job.params);
                    forced.uniform = Some(job.params);
                }
                _ => unreachable!(),
            }
            let label = format!("{change}, count_mode={count_mode}");
            let (out, work) = gpu.run_reused_views(&mut [(&mut retained, &job, inputs)], false);
            let (reference, reference_work) =
                gpu.run_reused_views(&mut [(&mut forced, &job, inputs)], true);
            assert_reuse_work(&label, &work[0], &job, true);
            assert_reuse_work(&label, &reference_work[0], &job, true);
            assert_reuse_output(&label, &out[0], &reference[0]);
            if change == "initial" {
                assert_exact(
                    &label,
                    &out[0],
                    &plan.bases,
                    &boundary_expected(&scene, &plan, |_| true),
                    count_mode,
                    true,
                );
                assert_survivors(
                    &label,
                    &out[0],
                    capacity - 1,
                    &(0..capacity - 1).collect::<Vec<_>>(),
                );
            }
            if change == "append count before planning" {
                assert_eq!(
                    out[0].list[capacity as usize - 1],
                    0,
                    "{label}: unplanned append"
                );
            } else if change == "plan appended instance" {
                assert!(
                    out[0].list[capacity as usize - 1] > 0,
                    "{label}: admitted append"
                );
            }
            if matches!(
                change,
                "initial" | "allocation replacement" | "view recreation"
            ) {
                assert!(
                    out[0].counts.iter().sum::<u32>() > 0,
                    "{label}: vacuous fixture"
                );
                assert_eq!(out[0].visible.last(), Some(&SENTINEL), "{label}: tail");
            } else if !change.contains("revision") && !change.starts_with("origin") {
                assert_ne!(
                    Some(&out[0]),
                    previous.as_ref(),
                    "{label}: mutation had no GPU effect"
                );
            }
            previous = out.into_iter().next();
            job.poison = false;
            for frame in 0..3 {
                let label = format!("{label}, unchanged frame {frame}");
                let (out, work) = gpu.run_reused_views(&mut [(&mut retained, &job, inputs)], false);
                let (reference, reference_work) =
                    gpu.run_reused_views(&mut [(&mut forced, &job, inputs)], true);
                assert_reuse_work(&label, &work[0], &job, false);
                assert_reuse_work(&label, &reference_work[0], &job, true);
                assert_reuse_output(&label, &out[0], &reference[0]);
                assert_eq!(
                    Some(&out[0]),
                    previous.as_ref(),
                    "{label}: retained bytes changed"
                );
            }
        }
    }
}

/// Mutating one view must neither invalidate nor clear another view's retained outputs.
/// A shared instance edit, in contrast, must invalidate both in its first frame.
#[test]
#[ignore = "hardware GPU proof: cargo nextest run -p engine --test cluster_stable_compaction --run-ignored only"]
fn stable_cull_reuse_keeps_view_allocations_independent() {
    let gpu = Gpu::new();
    let scene = boundary_scene(66, false, 3);
    let shared = gpu.shared(&scene);
    let groups = 4;
    let plan = scene.plan(scene.len(), groups, |id| scene.batch(id));
    let mut retained = [
        gpu.reuse_view(&shared, &scene, groups),
        gpu.reuse_view(&shared, &scene, groups),
    ];
    let mut forced = [
        gpu.reuse_view(&shared, &scene, groups),
        gpu.reuse_view(&shared, &scene, groups),
    ];
    let mut jobs: [Job; 2] = std::array::from_fn(|view| Job {
        params: view_params(scene.len(), groups as u32, 1, true),
        table: batch_table(&plan.group_entries(), scene.len(), |id| {
            id % 3 != view as u32
        }),
        order: plan.order(scene.len()),
        count_mode: view == 1,
        poison: true,
    });
    let mut inputs = [CullInputs::default(); 2];
    let mut previous: Option<Vec<Output>> = None;
    for (step, expected) in [
        [true, true],
        [false, false],
        [true, false],
        [false, true],
        [true, true],
        [true, false],
        [false, false],
    ]
    .into_iter()
    .enumerate()
    {
        match step {
            2 => jobs[0].params.error_pixels = 200.0,
            3 => {
                jobs[1].table = batch_table(&plan.group_entries(), scene.len(), |id| {
                    !id.is_multiple_of(3)
                });
                inputs[1].selection += 1;
            }
            4 => {
                // No per-view input changes: the shared revision alone must invalidate both.
                let id = 5;
                let transform = Mat4::from_translation(Vec3::new(-100.0, 0.0, 0.0));
                gpu.queue.write_buffer(
                    &shared.instances,
                    id * 20 * 4,
                    bytemuck::cast_slice(&transform.to_cols_array().map(f32::to_bits)),
                );
                for input in &mut inputs {
                    input.gpu += 1;
                }
            }
            5 => {
                retained[0] = gpu.reuse_view(&shared, &scene, groups);
                forced[0] = gpu.reuse_view(&shared, &scene, groups);
                retained[0].uniform = Some(jobs[0].params);
                forced[0].uniform = Some(jobs[0].params);
            }
            _ => {}
        }
        let mut runs: Vec<_> = retained
            .iter_mut()
            .zip(&jobs)
            .zip(inputs)
            .map(|((view, job), input)| (view, job, input))
            .collect();
        let (out, work) = gpu.run_reused_views(&mut runs, false);
        let mut runs: Vec<_> = forced
            .iter_mut()
            .zip(&jobs)
            .zip(inputs)
            .map(|((view, job), input)| (view, job, input))
            .collect();
        let (reference, reference_work) = gpu.run_reused_views(&mut runs, true);
        for view in 0..2 {
            let label = format!("multi-view step {step}, view {view}");
            assert_reuse_work(&label, &work[view], &jobs[view], expected[view]);
            assert_reuse_work(&label, &reference_work[view], &jobs[view], true);
            assert_reuse_output(&label, &out[view], &reference[view]);
            if !expected[view] {
                assert_eq!(
                    out[view],
                    previous.as_ref().unwrap()[view],
                    "{label}: retained bytes"
                );
            } else if (2..=4).contains(&step) {
                assert_ne!(
                    out[view],
                    previous.as_ref().unwrap()[view],
                    "{label}: mutation had no GPU effect"
                );
            }
        }
        if step == 0 {
            assert_ne!(
                out[0].visible, out[1].visible,
                "views must select different draws"
            );
            assert!(out.iter().all(|view| view.dispatch[3] > 0));
        }
        previous = Some(out);
        for job in &mut jobs {
            job.poison = false;
        }
    }
}

/// Subpixel projected size cannot remove otherwise selected geometry.
#[test]
#[ignore = "hardware GPU proof: cargo nextest run -p engine --test cluster_stable_compaction --run-ignored only"]
fn stable_tiny_instances_remain_selected() {
    let gpu = Gpu::new();
    let mut scene = boundary_scene(1, true, 1);
    scene.instances[..16].copy_from_slice(
        &Mat4::from_translation(Vec3::new(1000.0, 0.0, 0.0))
            .to_cols_array()
            .map(f32::to_bits),
    );
    let plan = scene.plan(1, 1, |id| scene.batch(id));
    let shared = gpu.shared(&scene);
    let stable = gpu.bind_stable(
        &shared,
        gpu.view(scene.slots_before(1), 1, 1, 1, INDEXED_ARG_WORDS),
    );
    let mut params = view_params(1, 1, 1, false);
    params.error_pixels = 0.00001;
    let expected = boundary_expected(&scene, &plan, |_| true);
    assert!(!expected[0].is_empty());
    for count_mode in [false, true] {
        let job = Job {
            params,
            table: batch_table(&plan.group_entries(), 1, |_| true),
            order: plan.order(1),
            count_mode,
            poison: true,
        };
        let output = gpu.run_stable(&stable, &job);
        assert_exact(
            "subpixel instance",
            &output,
            &plan.bases,
            &expected,
            count_mode,
            true,
        );
        assert_survivors("subpixel instance", &output, 1, &[0]);
    }
}
