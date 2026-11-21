//! Authored synthetic plugins through the converter, then the real mapped CPU generator.
//! Retain a GPU smoke pack by setting MUDCRAB_GRASS_FIXTURE_OUTPUT to a new output folder.
#[path = "support/grass_budget.rs"]
mod grass_budget;

use converter::{config::TextureEncoder, esm::EsmParser};
use dummy_content::{esm, layout, nif};
use engine::{
    config::EngineConfig,
    grass::{GrassInstance, generation::generate},
    world::grass_cache::GrassCache,
};
use std::{
    fs,
    hint::black_box,
    path::Path,
    time::{Duration, Instant},
};

// The performance test measures only synchronous generation, after
// conversion tasks have joined. Counts Rust heap allocations, not mmap or GPU memory.
struct CountingAllocator;
static LIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static PEAK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

// SAFETY: Allocation and deallocation delegate unchanged pointers and layouts to System;
// the additional atomic counters never access the allocated memory.
unsafe impl std::alloc::GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        // SAFETY: The caller supplies the valid allocation layout required by GlobalAlloc.
        let ptr = unsafe { std::alloc::System.alloc(layout) };
        if !ptr.is_null() {
            let live =
                LIVE.fetch_add(layout.size(), std::sync::atomic::Ordering::SeqCst) + layout.size();
            PEAK.fetch_max(live, std::sync::atomic::Ordering::SeqCst);
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        // SAFETY: The caller returns a live System allocation with its original layout.
        unsafe { std::alloc::System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), std::sync::atomic::Ordering::SeqCst);
    }
}

const GRASS_COUNT: u32 = 8;
const PATCH_GRASS_BASE: u32 = 0x0102_0000;
const PATCH_TEXTURE: u32 = 0x0102_0010;
const OBSTRUCTION_MODEL: &str = "meshes/bench-obstruction.nif";
const GRASS_ALPHA_THRESHOLD: u8 = 96;

/// Encode a fixture subrecord with its tag and 16-bit payload length.
fn sub(tag: &[u8], bytes: &[u8]) -> Vec<u8> {
    [tag, &(bytes.len() as u16).to_le_bytes(), bytes].concat()
}

/// Wrap a fixture payload in a Skyrim record header with the supplied FormID.
fn record(tag: &[u8; 4], id: u32, payload: Vec<u8>) -> Vec<u8> {
    [
        tag.as_slice(),
        &(payload.len() as u32).to_le_bytes(),
        &0u32.to_le_bytes(),
        &id.to_le_bytes(),
        &[0; 4],
        &44u16.to_le_bytes(),
        &[0; 2],
        &payload,
    ]
    .concat()
}

/// Wrap fixture records in a group with the supplied kind and label.
fn group(kind: u32, label: u32, payload: Vec<u8>) -> Vec<u8> {
    [
        b"GRUP".as_slice(),
        &(payload.len() as u32 + 24).to_le_bytes(),
        &label.to_le_bytes(),
        &kind.to_le_bytes(),
        &[0; 8],
        &payload,
    ]
    .concat()
}

/// Add grass rules and blended quadrants as an actual master-dependent plugin.
fn grass_patch(base: &Path) -> Vec<u8> {
    let records = EsmParser::merge_plugins(&[base.to_owned()]).unwrap();
    let texture = records
        .values()
        .find(|record| record.record_type == *b"LTEX")
        .unwrap();
    let mut bytes = record(
        b"TES4",
        0,
        [sub(b"MAST", b"Skyrim.esm\0"), sub(b"DATA", &[0; 8])].concat(),
    );
    for index in 0..GRASS_COUNT {
        let mut data = vec![0; 32];
        data[0] = 100;
        data[2] = 90;
        // Position Range 1 keeps the minimum-spacing grid at every tested spacing; retail rule
        // (RF-GRASS-012) places nothing for a range of exactly 0.
        for (offset, value) in [(12, 1.0f32), (16, 0.35), (20, 0.2), (24, 2.5)] {
            data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        data[28] = 5; // Vertex lighting and fit to slope; preserve authored nonuniform height.
        bytes.extend(record(
            b"GRAS",
            PATCH_GRASS_BASE + index,
            [
                sub(b"EDID", format!("BenchGrass{index}\0").as_bytes()),
                sub(b"MODL", b"grass/bench.nif\0"),
                sub(b"DATA", &data),
            ]
            .concat(),
        ));
    }
    for (id, first) in [(texture.form_id, 0), (PATCH_TEXTURE, 4)] {
        let mut payload: Vec<u8> = texture
            .subrecords
            .iter()
            .filter(|(tag, _)| tag != b"GNAM")
            .flat_map(|(tag, value)| sub(tag, value))
            .collect();
        for index in first..first + 4 {
            payload.extend(sub(b"GNAM", &(PATCH_GRASS_BASE + index).to_le_bytes()));
        }
        bytes.extend(record(b"LTEX", id, payload));
    }
    let mut land: Vec<_> = records
        .values()
        .filter(|record| record.record_type == *b"LAND")
        .collect();
    land.sort_unstable_by_key(|record| record.form_id);
    let mut children = Vec::new();
    for source in land {
        let mut payload: Vec<u8> = source
            .subrecords
            .iter()
            .filter(|(tag, _)| tag != b"BTXT" && tag != b"ATXT" && tag != b"VTXT")
            .flat_map(|(tag, value)| sub(tag, value))
            .collect();
        for quadrant in 0u8..4 {
            payload.extend(sub(
                b"BTXT",
                &[
                    texture.form_id.to_le_bytes().as_slice(),
                    &[quadrant, 0, 0, 0],
                ]
                .concat(),
            ));
            payload.extend(sub(
                b"ATXT",
                &[PATCH_TEXTURE.to_le_bytes().as_slice(), &[quadrant, 0, 0, 0]].concat(),
            ));
            let mut weights = Vec::new();
            for vertex in 0u16..289 {
                weights.extend(vertex.to_le_bytes());
                weights.extend([0; 2]);
                weights.extend((0.1f32 + 0.7 * f32::from(vertex % 17) / 16.0).to_le_bytes());
            }
            payload.extend(sub(b"VTXT", &weights));
        }
        children.extend(group(
            6,
            source.cell_form_id.unwrap(),
            record(b"LAND", source.form_id, payload),
        ));
    }
    bytes.extend(group(1, 1, children));
    bytes
}

/// Real NIF crossed planes and a DDS with opaque blades and transparent background.
/// The explicit alpha-test property exercises textured cutouts. This fixture has
/// no vertex colors; fractional vertex-alpha GPU coverage requires a separate
/// renderer fixture and is not established by this performance test.
fn grass_mesh(data: &Path) {
    let positions = [
        [-20.0, 0.0, 0.0],
        [20.0, 0.0, 0.0],
        [20.0, 0.0, 80.0],
        [-20.0, 0.0, 80.0],
        [0.0, -20.0, 0.0],
        [0.0, 20.0, 0.0],
        [0.0, 20.0, 80.0],
        [0.0, -20.0, 80.0],
    ];
    let normals = [[0.0, -1.0, 0.0]; 4]
        .into_iter()
        .chain([[1.0, 0.0, 0.0]; 4])
        .collect::<Vec<_>>();
    let uvs = [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]].repeat(2);
    let mesh = nif::static_shape_with_alpha_test(
        &nif::StaticShape {
            name: "BenchGrass",
            positions: &positions,
            normals: &normals,
            uvs: &uvs,
            indices: &[[0, 1, 2], [0, 2, 3], [4, 5, 6], [4, 6, 7]],
            diffuse: "textures/grass/bench.dds",
            normal_texture: layout::GENERATED_NORMAL_PATH,
        },
        GRASS_ALPHA_THRESHOLD,
    )
    .unwrap();
    fs::create_dir_all(data.join("meshes/grass")).unwrap();
    fs::create_dir_all(data.join("textures/grass")).unwrap();
    fs::write(data.join("meshes/grass/bench.nif"), mesh).unwrap();
    let mut texture = ddsfile::Dds::new_d3d(ddsfile::NewD3dParams {
        width: 32,
        height: 32,
        depth: None,
        format: ddsfile::D3DFormat::A8R8G8B8,
        mipmap_levels: Some(1),
        caps2: None,
    })
    .unwrap();
    for (index, rgba) in texture.data.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let x = index % 32;
        let y = index / 32;
        let blade = (x.abs_diff(8) <= 2 || x.abs_diff(17) <= 2 || x.abs_diff(25) <= 1) && y > 3;
        rgba.copy_from_slice(&[30, 125, 60, if blade { 255 } else { 0 }]); // BGRA
    }
    let file = fs::File::create(data.join("textures/grass/bench.dds")).unwrap();
    texture.write(&mut std::io::BufWriter::new(file)).unwrap();
}

/// Check that the converted grass primitives use the authored alpha-test cutoff.
fn assert_grass_material(output: &Path) {
    let glb = fs::read(output.join("meshes/grass/bench.glb")).unwrap();
    assert_eq!(&glb[..4], b"glTF");
    assert_eq!(&glb[16..20], b"JSON");
    let json_length = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
    let json: serde_json::Value = serde_json::from_slice(&glb[20..20 + json_length]).unwrap();
    let materials = json["materials"].as_array().unwrap();
    assert_eq!(materials.len(), 1, "the grass fixture has one material");
    assert_eq!(
        materials[0]["alphaMode"], "MASK",
        "converted grass must exercise texture alpha cutouts"
    );
    let cutoff = materials[0]["alphaCutoff"].as_f64().unwrap();
    assert!(
        (cutoff - f64::from(GRASS_ALPHA_THRESHOLD) / 255.0).abs() < 1e-6,
        "converted cutoff must preserve the authored threshold: {cutoff}"
    );
    let meshes = json["meshes"].as_array().unwrap();
    assert!(!meshes.is_empty());
    for mesh in meshes {
        let primitives = mesh["primitives"].as_array().unwrap();
        assert!(!primitives.is_empty());
        for primitive in primitives {
            assert_eq!(primitive["material"].as_u64(), Some(0));
        }
    }
}

/// A closed opaque box around the cell-center reference, distinct from grass.
/// Its 128-unit footprint covers sixteen 32-unit mask sample centers; the
/// layout's default two-unit quad falls between those samples and blocks none.
fn obstruction_mesh(data: &Path) {
    let positions = [
        [-64.0, -64.0, 0.0],
        [64.0, -64.0, 0.0],
        [64.0, 64.0, 0.0],
        [-64.0, 64.0, 0.0],
        [-64.0, -64.0, 96.0],
        [64.0, -64.0, 96.0],
        [64.0, 64.0, 96.0],
        [-64.0, 64.0, 96.0],
    ];
    let normals = [[0.0, 0.0, -1.0]; 4]
        .into_iter()
        .chain([[0.0, 0.0, 1.0]; 4])
        .collect::<Vec<_>>();
    let uvs = [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]].repeat(2);
    let mesh = nif::static_shape(&nif::StaticShape {
        name: "BenchObstruction",
        positions: &positions,
        normals: &normals,
        uvs: &uvs,
        indices: &[
            [0, 2, 1],
            [0, 3, 2], // Bottom.
            [4, 5, 6],
            [4, 6, 7], // Top.
            [0, 1, 5],
            [0, 5, 4],
            [1, 2, 6],
            [1, 6, 5],
            [2, 3, 7],
            [2, 7, 6],
            [3, 0, 4],
            [3, 4, 7],
        ],
        diffuse: layout::GENERATED_DIFFUSE_PATH,
        normal_texture: layout::GENERATED_NORMAL_PATH,
    })
    .unwrap();
    fs::write(data.join(OBSTRUCTION_MODEL), mesh).unwrap();
}

/// Median of timing samples. An even count averages the two middle samples.
fn sample_median(samples: &[Duration]) -> Duration {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[middle]
    } else {
        (sorted[middle - 1] + sorted[middle]) / 2
    }
}

/// Judge a cell on its first samples, re-measuring once when the median misses.
///
/// A first-pass median within `ceiling` decides the cell immediately. A miss spends
/// the single re-measure, adding five fresh samples, and the median of all eight
/// decides. Returns the judged median, whether it is within budget, and whether a
/// re-measure happened.
fn judged_median(
    first: &[Duration],
    remeasure: impl FnOnce() -> Vec<Duration>,
    ceiling: Duration,
) -> (Duration, bool, bool) {
    let first_median = sample_median(first);
    if first_median < ceiling {
        return (first_median, true, false);
    }
    let mut all = first.to_vec();
    all.extend(remeasure());
    let judged = sample_median(&all);
    (judged, judged < ceiling, true)
}

fn millis(value: u64) -> Duration {
    Duration::from_millis(value)
}

#[test]
/// A cell within budget on the first pass is never re-measured.
fn judged_median_keeps_a_fast_first_pass() {
    let first = [millis(1), millis(3), millis(2)];
    let (median, within_budget, remeasured) = judged_median(
        &first,
        || panic!("a cell within budget must not be re-measured"),
        millis(10),
    );
    assert_eq!(median, millis(2));
    assert!(within_budget);
    assert!(!remeasured);
}

#[test]
/// A one-off miss that recovers on the second measurement passes on all eight samples.
fn judged_median_accepts_a_miss_that_recovers() {
    let first = [millis(24), millis(20), millis(22)];
    let mut remeasures = 0;
    let (median, within_budget, remeasured) = judged_median(
        &first,
        || {
            remeasures += 1;
            vec![millis(1), millis(2), millis(3), millis(4), millis(6)]
        },
        millis(10),
    );
    assert_eq!(remeasures, 1, "exactly one re-measure per cell");
    assert!(remeasured);
    assert!(within_budget, "median of all eight samples is {median:?}");
    assert_eq!(median, millis(5));
}

#[test]
/// A real regression stays slow on the re-measure and still fails the budget.
fn judged_median_fails_a_slow_remeasure() {
    let first = [millis(24), millis(20), millis(22)];
    let mut remeasures = 0;
    let (median, within_budget, remeasured) = judged_median(
        &first,
        || {
            remeasures += 1;
            vec![millis(18), millis(19), millis(20), millis(21), millis(23)]
        },
        millis(10),
    );
    assert_eq!(remeasures, 1, "exactly one re-measure per cell");
    assert!(remeasured);
    assert!(!within_budget, "median of all eight samples is {median:?}");
    assert_eq!(median, Duration::from_micros(20_500));
}

/// A slowdown present in only half of the eight samples passes: the median of eight is the mean
/// of the fourth and fifth samples. This is the documented limit of the single re-measure.
#[test]
fn judged_median_accepts_a_mixed_remeasure_with_half_the_samples_slow() {
    let slow = Duration::from_micros(9_050);
    let fast = Duration::from_micros(4_930);
    let first = [slow, slow, slow];
    let (median, within_budget, remeasured) =
        judged_median(&first, || vec![slow, fast, fast, fast, fast], millis(9));
    assert!(remeasured);
    assert_eq!(median, Duration::from_micros(6_990));
    assert!(
        within_budget,
        "four of eight samples over budget still pass on the median"
    );
}

/// Check bounded deterministic generation and report each cell's median budget result.
fn measured(
    cache: &GrassCache,
    ids: &[u32],
    spacing: f32,
    slots: u32,
    strict_ceiling: Duration,
) -> bool {
    let allowance = grass_budget::grass_budget_allowance();
    let ceiling = strict_ceiling.mul_f64(allowance);
    let config = EngineConfig {
        grass: true,
        grass_min_spacing: spacing,
        grass_max_types_per_texture: 4,
        ..EngineConfig::default()
    };
    let mut durations = Vec::new();
    let mut total_instances = 0usize;
    let mut timing_passed = true;
    for &id in ids {
        let baseline = LIVE.load(std::sync::atomic::Ordering::SeqCst);
        PEAK.store(baseline, std::sync::atomic::Ordering::SeqCst);
        let warm = generate(cache, id, &config);
        let peak = PEAK
            .load(std::sync::atomic::Ordering::SeqCst)
            .saturating_sub(baseline);
        let retained = LIVE
            .load(std::sync::atomic::Ordering::SeqCst)
            .saturating_sub(baseline);
        let instance_bytes = std::mem::size_of::<GrassInstance>();
        // The unpublished Z lane holds binary16 height within each 16-byte instance.
        let staging_bytes = instance_bytes;
        let potential_instances = slots as usize * GRASS_COUNT as usize;
        let peak_budget = potential_instances * 48 + 64 * 1024;
        let retained_budget = potential_instances * instance_bytes + 64 * 1024;
        // All species' growing vectors coexist before compaction. Allow doubled staging
        // capacity minus retained output, plus one buffer's growth/shrink overlap.
        let scratch_per_slot =
            GRASS_COUNT as usize * (2 * staging_bytes - instance_bytes) + staging_bytes;
        let scratch_budget = slots as usize * scratch_per_slot + 64 * 1024;
        eprintln!(
            "spacing={spacing} cell={id} peak_heap_bytes={peak} retained_heap_bytes={retained} scratch_heap_bytes={} peak_budget_bytes={peak_budget} retained_budget_bytes={retained_budget} scratch_budget_bytes={scratch_budget}",
            peak.saturating_sub(retained)
        );
        assert!(
            peak <= peak_budget,
            "generation peak heap budget exceeded: {peak}"
        );
        assert!(
            retained <= retained_budget,
            "generation retained heap budget exceeded: {retained}"
        );
        assert!(
            peak.saturating_sub(retained) <= scratch_budget,
            "generation scratch heap budget exceeded: peak={peak}, retained={retained}"
        );
        assert_eq!(
            warm.batches.len(),
            GRASS_COUNT as usize,
            "all blended species must participate"
        );
        assert_eq!(
            warm.batches
                .iter()
                .map(|batch| batch.grass_id)
                .collect::<Vec<_>>(),
            (0..8).map(|index| 0x0102_0000 + index).collect::<Vec<_>>(),
            "post-add cap four admits all four GNAM entries of each texture"
        );
        let count: usize = warm.batches.iter().map(|batch| batch.instances.len()).sum();
        assert!(
            count > 500 && count <= slots as usize * GRASS_COUNT as usize,
            "nonvacuous bounded output: {count}"
        );
        assert!(
            warm.bytes <= slots as usize * GRASS_COUNT as usize * 32 + 32 * 1024,
            "cell allocation budget exceeded: {}",
            warm.bytes
        );
        total_instances += count;
        for batch in &warm.batches {
            assert_eq!(
                batch.instances.capacity(),
                batch.instances.len(),
                "published instances retain exact capacity"
            );
            assert_eq!(batch.slot_count, 262144);
            assert!(
                batch
                    .instances
                    .iter()
                    .all(|instance| instance.candidate_slot() < 262144)
            );
            assert!(
                batch
                    .instances
                    .windows(2)
                    .all(|pair| pair[0].candidate_slot() <= pair[1].candidate_slot())
            );
        }
        let sample = |count: usize| {
            let mut samples = Vec::with_capacity(count);
            for _ in 0..count {
                let start = Instant::now();
                let result = black_box(generate(cache, id, black_box(&config)));
                samples.push(start.elapsed());
                assert_eq!(warm.batches.len(), result.batches.len());
                for (expected, actual) in warm.batches.iter().zip(&result.batches) {
                    assert_eq!(expected.grass_id, actual.grass_id);
                    assert_eq!(
                        expected.instances, actual.instances,
                        "replayed cell must be deterministic"
                    );
                }
            }
            samples
        };
        let first = sample(3);
        let first_median = sample_median(&first);
        let (median, within_budget, remeasured) = judged_median(&first, || sample(5), ceiling);
        timing_passed &= within_budget;
        if remeasured {
            eprintln!(
                "grass cell generation re-measured: spacing={spacing}, cell={id:08X}, first_median={first_median:?}, samples=8, median={median:?}, strict_budget={strict_ceiling:?}, allowance={allowance}, effective_budget={ceiling:?}, within_budget={within_budget}"
            );
        }
        eprintln!(
            "grass cell generation: spacing={spacing}, cell={id:08X}, median={median:?}, strict_budget={strict_ceiling:?}, allowance={allowance}, effective_budget={ceiling:?}, within_budget={within_budget}",
        );
        durations.push(median);
    }
    durations.sort_unstable();
    eprintln!(
        "grass generation: spacing={spacing}, cells={}, slots/cell={slots}, instances={total_instances}, median={:?}, worst-cell-median={:?}, strict_budget={strict_ceiling:?}, allowance={allowance}, effective_budget={ceiling:?}",
        ids.len(),
        durations[durations.len() / 2],
        durations.last().unwrap()
    );
    timing_passed
}

#[tokio::test]
#[ignore = "performance"]
/// Convert synthetic grass terrain and enforce CPU generation budgets at three densities.
async fn performance_grass_generation() {
    assert_eq!(
        env!("MUDCRAB_BUILD_OPT_LEVEL"),
        "3",
        "run budgets with --release at opt-level=3"
    );
    assert_eq!(
        std::mem::size_of::<GrassInstance>(),
        16,
        "GPU upload contract stays quantized"
    );
    let temporary = tempfile::tempdir().unwrap();
    let data = temporary.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    let mut formats = layout::Formats::all();
    formats.bsa = false;
    formats.ba2 = false;
    formats.pex = false;
    layout::generate(&data, layout::DEFAULT_SEED, formats).unwrap();
    obstruction_mesh(&data);
    // The runtime streams radius two while grass measures its inner radius-one
    // ring. Supply all 25 terrain cells so retained GPU packs can reach readiness.
    let terrain_cells: Vec<_> = (-2..=2)
        .flat_map(|grid_y| (-2..=2).map(move |grid_x| esm::Cell { grid_x, grid_y }))
        .collect();
    let base = esm::plugin(&esm::Plugin {
        author: layout::GENERATED_AUTHOR,
        worldspace: layout::GENERATED_WORLDSPACE,
        cells: &terrain_cells,
        model_path: OBSTRUCTION_MODEL,
        diffuse: layout::GENERATED_DIFFUSE_PATH,
        normal_texture: layout::GENERATED_NORMAL_PATH,
    })
    .unwrap();
    layout::write_plugin(&data, &base).unwrap();
    fs::write(
        data.join("Grass.esp"),
        grass_patch(&data.join("Skyrim.esm")),
    )
    .unwrap();
    grass_mesh(&data);
    let output = std::env::var_os("MUDCRAB_GRASS_FIXTURE_OUTPUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temporary.path().join("modern"));
    let mut config = converter::PipelineConfig::new(&data, &output);
    // This CPU budget fixture must not initialize the default GPU LOD encoder.
    config.texture_encoder = TextureEncoder::Cpu;
    config.lod_texture_encoder = TextureEncoder::Cpu;
    config.cpu_jobs = 2;
    config.io_jobs = 2;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = converter::AssetPipeline::run_async(config, tx)
        .await
        .unwrap();
    drain.await.unwrap();
    assert!(report.complete, "synthetic conversion must complete");
    assert!(output.join("meshes/grass/bench.glb").is_file());
    assert_grass_material(&output);
    let second_output = temporary.path().join("modern-single-job");
    let mut second_config = converter::PipelineConfig::new(&data, &second_output);
    second_config.texture_encoder = TextureEncoder::Cpu;
    second_config.lod_texture_encoder = TextureEncoder::Cpu;
    second_config.cpu_jobs = 1;
    second_config.io_jobs = 1;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let second_report = converter::AssetPipeline::run_async(second_config, tx)
        .await
        .unwrap();
    drain.await.unwrap();
    assert!(second_report.complete);
    assert_eq!(
        fs::read(output.join("grass_cache.rkyv")).unwrap(),
        fs::read(second_output.join("grass_cache.rkyv")).unwrap(),
        "independent conversions at different job counts must produce identical grass caches"
    );
    let cache = GrassCache::open(&output.join("grass_cache.rkyv")).unwrap();
    for cell in &terrain_cells {
        assert!(
            cache.cell_at(1, cell.grid_x, cell.grid_y).is_some(),
            "the full radius-two terrain ring must survive conversion"
        );
    }
    let cells: Vec<_> = (-1..=1)
        .flat_map(|y| (-1..=1).map(move |x| (x, y)))
        .map(|(x, y)| {
            cache
                .cell_at(1, x, y)
                .expect("all nine authored cells must be cached")
        })
        .collect();
    for &id in &cells {
        let cell = cache.cell(id).unwrap();
        assert!(
            cell.quadrants
                .iter()
                .all(|quadrant| quadrant.layers.len() == 2)
        );
        let blocked_tiles: u32 = cell
            .quadrants
            .iter()
            .flat_map(|quadrant| quadrant.exclusion.iter())
            .map(|byte| byte.count_ones())
            .sum();
        assert_eq!(
            blocked_tiles, 16,
            "the converted opaque box must block its sixteen upward-ray mask samples"
        );
    }
    // CI runners vary; the strict budget applies to local and release runs.
    // The printed measurements, not these worker-latency ceilings, describe a machine's
    // observed performance; generation never runs in the frame's cell commit.
    let mut timing_passed = measured(&cache, &cells, 64.0, 4096, Duration::from_millis(6));
    timing_passed &= measured(&cache, &cells, 32.0, 16384, Duration::from_millis(25));
    timing_passed &= measured(&cache, &cells, 20.0, 36864, Duration::from_millis(60));
    assert!(
        timing_passed,
        "grass generation timing budget exceeded; see per-cell medians above"
    );
    if std::env::var_os("MUDCRAB_GRASS_FIXTURE_OUTPUT").is_some() {
        eprintln!(
            "retained synthetic grass runtime pack: {}",
            output.display()
        );
    }
}
