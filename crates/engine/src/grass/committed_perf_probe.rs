//! Ignored byte probe for the retained synthetic cache and the actual committed-terrain generator.
use super::{GeneratedGrass, generate, generate_on_terrain, quantize, sampling};
use crate::{
    config::EngineConfig,
    grass::GrassTerrain,
    world::{cache::TerrainSnapshot, grass_cache::GrassCache},
};
use std::{fs, io::Write, path::Path};

fn dump(out: &Path, stem: &str, result: &GeneratedGrass) -> std::io::Result<usize> {
    let mut bytes = fs::File::create(out.join(format!("{stem}.bin")))?;
    let mut metadata = format!(
        "retained_bytes={} batches={}\n",
        result.bytes,
        result.batches.len()
    );
    let mut count = 0;
    for batch in &result.batches {
        for word in [
            batch.cell_id,
            batch.grass_id,
            batch.height_min.to_bits(),
            batch.height_span.to_bits(),
            batch.slot_count,
            batch.wave_period.to_bits(),
            u32::from(batch.flags),
            batch.instances.len() as u32,
        ] {
            bytes.write_all(&word.to_le_bytes())?;
        }
        bytes.write_all(bytemuck::cast_slice(batch.instances.as_slice()))?;
        metadata.push_str(&format!(
            "grass={} grid={:?} model={:?} count={} capacity={}\n",
            batch.grass_id,
            batch.grid,
            batch.model_path,
            batch.instances.len(),
            batch.instances.capacity(),
        ));
        count += batch.instances.len();
    }
    fs::write(out.join(format!("{stem}.metadata.txt")), metadata)?;
    assert!(count > 0, "fixture must generate instances for {stem}");
    Ok(count)
}

/// A test-only input snapshot carries the mapped colour/water fields through the real adapter.
fn snapshot(cache: &GrassCache, cell_id: u32, modified: bool) -> TerrainSnapshot {
    let cell = cache.cell(cell_id).unwrap();
    let mut heights: Vec<f32> = cell.heights.iter().map(|value| (*value).into()).collect();
    if modified {
        for (i, value) in heights.iter_mut().enumerate() {
            // Fractional asymmetric vertices distinguish committed geometry from binary16 selection.
            *value += if (i % 33) % 2 == 1 && (i / 33) % 2 == 1 {
                5.5
            } else {
                -1.75
            };
        }
    }
    TerrainSnapshot {
        cell_id,
        width: 33,
        height: 33,
        heights,
        normals: cell.normals.to_vec(),
        vertex_colors: cell.vertex_colors.to_vec(),
        layers: vec![],
        water_height: cell.water_height.as_ref().map(|height| (*height).into()),
        water_type_form_id: None,
    }
}

/// Capture actual baked and committed runtime output before and after optimization.
#[test]
#[ignore = "requires retained synthetic grass cache and output directory"]
fn dump_committed_fixture_bytes() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(env!("MUDCRAB_BUILD_OPT_LEVEL"), "3");
    let input = std::env::var("MUDCRAB_GRASS_PROBE_CACHE")?;
    let output = std::env::var("MUDCRAB_GRASS_PROBE_OUTPUT")?;
    let out = Path::new(&output);
    fs::create_dir_all(out)?;
    let cache = GrassCache::open(Path::new(&input))?;
    let mut baked_cases = 0;
    let mut runtime_cases = 0;
    let mut grounded_instances = 0;
    for spacing in [64.0, 32.0, 20.0] {
        let config = EngineConfig {
            grass: true,
            grass_min_spacing: spacing,
            grass_max_types_per_texture: 4,
            ..Default::default()
        };
        for y in -1..=1 {
            for x in -1..=1 {
                let id = cache.cell_at(1, x, y).unwrap();
                let baked = generate(&cache, id, &config);
                let count = dump(out, &format!("baked-s{spacing}-cell{id:08x}"), &baked)?;
                baked_cases += 1;
                println!("baked spacing={spacing} cell={id:08x} instances={count}");
                for modified in [false, true] {
                    let snapshot = snapshot(&cache, id, modified);
                    let terrain = GrassTerrain::from_snapshot(&snapshot);
                    let generated = generate_on_terrain(&cache, id, &config, &terrain);
                    let minimum = snapshot
                        .heights
                        .iter()
                        .copied()
                        .fold(f32::INFINITY, f32::min);
                    let maximum = snapshot
                        .heights
                        .iter()
                        .copied()
                        .fold(f32::NEG_INFINITY, f32::max);
                    let span = (maximum - minimum).max(1.0);
                    for batch in &generated.batches {
                        assert_eq!(batch.height_min.to_bits(), minimum.to_bits());
                        assert_eq!(batch.height_span.to_bits(), span.to_bits());
                        for instance in batch.instances.iter() {
                            let [px, py] = [instance.position[0], instance.position[1]]
                                .map(|value| f32::from(value) * 4096.0 / 65535.0);
                            let height = sampling::committed_height(px, py, &snapshot.heights);
                            assert_eq!(instance.position[2], quantize(height - minimum, span));
                            grounded_instances += 1;
                        }
                    }
                    let count = dump(
                        out,
                        &format!("runtime-modified{modified}-s{spacing}-cell{id:08x}"),
                        &generated,
                    )?;
                    runtime_cases += 1;
                    println!(
                        "runtime modified={modified} spacing={spacing} cell={id:08x} instances={count}"
                    );
                }
            }
        }
    }
    assert_eq!(baked_cases, 27);
    assert_eq!(runtime_cases, 54);
    assert!(grounded_instances > 0);
    println!(
        "byte probe complete baked={baked_cases} runtime={runtime_cases} grounded_instances={grounded_instances}"
    );
    Ok(())
}
