//! CPU timing and packed-byte diagnostics for the converted grass performance fixture.
use engine::grass::generation;
use engine::{config, world};
use std::{fs, hint::black_box, io::Write, time::Instant};
/// Measure production generation and dump packed output for the nine central fixture cells.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 {
        return Err("usage: grass_perf_probe GRASS_CACHE OUTPUT_DIRECTORY".into());
    }
    let cache = world::grass_cache::GrassCache::open(std::path::Path::new(&args[1]))?;
    let out = std::path::Path::new(&args[2]);
    fs::create_dir_all(out)?;
    for spacing in [64.0, 32.0, 20.0] {
        let config = config::EngineConfig {
            grass: true,
            grass_min_spacing: spacing,
            grass_max_types_per_texture: 4,
            ..Default::default()
        };
        for y in -1..=1 {
            for x in -1..=1 {
                let id = cache.cell_at(1, x, y).unwrap();
                let mut times = Vec::new();
                let mut count = 0;
                for rep in 0..8 {
                    let start = Instant::now();
                    let batches = generation::generate(&cache, id, black_box(&config)).batches;
                    let elapsed = start.elapsed();
                    count = batches.iter().map(|b| b.instances.len()).sum::<usize>();
                    if rep > 0 {
                        times.push(elapsed);
                    }
                    if rep == 0 {
                        let mut bytes =
                            fs::File::create(out.join(format!("v7-s{spacing}-cell{id:08x}.bin")))?;
                        for b in &batches {
                            for v in [
                                b.cell_id,
                                b.grass_id,
                                b.height_min.to_bits(),
                                b.height_span.to_bits(),
                                b.slot_count,
                                b.wave_period.to_bits(),
                                b.flags as u32,
                                b.instances.len() as u32,
                            ] {
                                bytes.write_all(&v.to_le_bytes())?;
                            }
                            bytes.write_all(bytemuck::cast_slice(b.instances.as_slice()))?;
                        }
                    }
                    black_box(batches);
                }
                times.sort_unstable();
                println!(
                    "G7 spacing={spacing} cell={id:08x} instances={count} median_ms={:.6}",
                    times[3].as_secs_f64() * 1000.0
                );
            }
        }
    }
    Ok(())
}
