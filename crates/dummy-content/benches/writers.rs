//! Throughput benchmarks for the fixture writers.

use criterion::{Criterion, criterion_group, criterion_main};
use dummy_content::{Entry, ba2, bsa, dds, rng::Rng};
use std::hint::black_box;

fn payloads(count: usize) -> Vec<(String, Vec<u8>)> {
    (0..count)
        .map(|index| (format!("assets/file{index}.bin"), vec![index as u8; 256]))
        .collect()
}

fn as_entries(payloads: &[(String, Vec<u8>)]) -> Vec<Entry<'_>> {
    payloads
        .iter()
        .map(|(name, data)| Entry::new(name, data))
        .collect()
}

fn bench_bsa(criterion: &mut Criterion) {
    let small = payloads(1_000);
    let small_entries = as_entries(&small);
    let large = payloads(10_000);
    let large_entries = as_entries(&large);
    let mut group = criterion.benchmark_group("bsa_v105");
    group.bench_function("uncompressed_1k", |bencher| {
        bencher.iter(|| black_box(bsa::v105(&small_entries, bsa::Compression::None).unwrap()))
    });
    group.bench_function("uncompressed_10k", |bencher| {
        bencher.iter(|| black_box(bsa::v105(&large_entries, bsa::Compression::None).unwrap()))
    });
    group.bench_function("zlib_1k", |bencher| {
        bencher.iter(|| black_box(bsa::v105(&small_entries, bsa::Compression::Zlib).unwrap()))
    });
    group.finish();
}

fn bench_ba2(criterion: &mut Criterion) {
    let payloads = payloads(1_000);
    let entries = as_entries(&payloads);
    let mut group = criterion.benchmark_group("ba2_gnrl");
    group.bench_function("uncompressed_1k", |bencher| {
        bencher.iter(|| black_box(ba2::general(&entries, ba2::Compression::None).unwrap()))
    });
    group.bench_function("zlib_1k", |bencher| {
        bencher.iter(|| black_box(ba2::general(&entries, ba2::Compression::Zlib).unwrap()))
    });
    group.finish();
}

fn bench_dds(criterion: &mut Criterion) {
    let spec = dds::Spec::new(dds::Format::Bc1Unorm, 64, 64).with_mip_levels(7);
    criterion.bench_function("dds_bc1_64_mipped", |bencher| {
        bencher.iter(|| black_box(dds::generate(&spec, &mut Rng::new(1)).unwrap()))
    });
}

fn bench_esm(criterion: &mut Criterion) {
    let cells: Vec<dummy_content::esm::Cell> = (0..81)
        .map(|index| dummy_content::esm::Cell {
            grid_x: (index % 9) - 4,
            grid_y: (index / 9) - 4,
        })
        .collect();
    let spec = dummy_content::esm::Plugin {
        author: "OpenSkyrim dummy-content",
        worldspace: "BenchWorld",
        cells: &cells,
        model_path: "meshes/generated.nif",
        diffuse: "textures/generated_color.dds",
        normal_texture: "textures/generated_normal.dds",
    };
    criterion.bench_function("esm_plugin_81_cells", |bencher| {
        bencher.iter(|| black_box(dummy_content::esm::plugin(&spec).unwrap()))
    });
}

criterion_group!(benches, bench_bsa, bench_ba2, bench_dds, bench_esm);
criterion_main!(benches);
