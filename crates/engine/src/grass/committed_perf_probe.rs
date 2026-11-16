//! Ignored byte probe for the retained synthetic cache and the actual committed-terrain generator.
use super::{GeneratedGrass, generate, generate_on_terrain};
use crate::{
    config::EngineConfig,
    grass::GrassTerrain,
    world::{cache::TerrainSnapshot, grass_cache::GrassCache},
};
use std::{fs, io::Write, path::Path};

/// Retain every batch header, packed instance and stable metadata field.
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

/// Carry mapped heights/water and synthetic VCLR through the real committed adapter.
fn snapshot(cache: &GrassCache, cell_id: u32, modified: bool) -> TerrainSnapshot {
    let cell = cache.cell(cell_id).unwrap();
    let mut heights: Vec<f32> = cell.heights.iter().map(|value| (*value).into()).collect();
    if modified {
        for (i, value) in heights.iter_mut().enumerate() {
            // Fractional asymmetric vertices exercise the supplied committed height field.
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
        normals: vec![0; 33 * 33 * 3],
        vertex_colors: (0..1089)
            .flat_map(|i| {
                [
                    (i % 33 * 7) as u8,
                    (i / 33 * 5) as u8,
                    ((i % 33 + i / 33) * 3) as u8,
                ]
            })
            .collect(),
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
    let mut runtime_instances = 0;
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
                println!("G6 baked spacing={spacing} cell={id:08x} instances={count}");
                for modified in [false, true] {
                    let snapshot = snapshot(&cache, id, modified);
                    let terrain = GrassTerrain::from_snapshot(&snapshot);
                    assert!(
                        terrain.luminance().is_some(),
                        "retain the synthetic committed VCLR plane"
                    );
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
                        // G6 samples the unquantized final root before packing XY and Z.
                        runtime_instances += batch.instances.len();
                    }
                    let count = dump(
                        out,
                        &format!("runtime-modified{modified}-s{spacing}-cell{id:08x}"),
                        &generated,
                    )?;
                    runtime_cases += 1;
                    println!(
                        "G6 runtime modified={modified} spacing={spacing} cell={id:08x} instances={count}"
                    );
                }
            }
        }
    }
    assert_eq!(baked_cases, 27);
    assert_eq!(runtime_cases, 54);
    assert!(runtime_instances > 0);
    println!(
        "G6 byte probe complete baked={baked_cases} runtime={runtime_cases} runtime_instances={runtime_instances}"
    );
    Ok(())
}
