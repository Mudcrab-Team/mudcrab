use super::*;
use shared::grass::{GRASS_CACHE_VERSION, GrassCell, GrassLayer, GrassQuadrant, GrassType};

/// Diagnostics observe all selections and leave the production stream and batches unchanged.
#[test]
fn observer_accounts_for_mask_without_changing_production() {
    let mut source = cell(vec![layer(vec![7], 1.0, true)]);
    source.quadrants[0].exclusion.fill(255);
    let cache = mapped(vec![rule(7, 128.0)], source);
    let config = EngineConfig::default();
    let plain = generate(&cache, 42, &config);
    let mut selected = 0;
    let mut blocked = 0;
    let observed = generate_observed(&cache, 42, &config, |p| {
        selected += 1;
        blocked += usize::from(p.obstructed);
        assert!(p.is_base);
        assert_eq!(p.form_id, 7);
    });
    assert_eq!(selected, 1024);
    assert_eq!(blocked, 256);
    assert_eq!(plain.batches[0].instances, observed.batches[0].instances);
    assert_eq!(plain.batches[0].instances.len(), selected - blocked);
}

/// One flat synthetic cell with no proprietary records or assets.
fn cell(layers: Vec<GrassLayer>) -> GrassCell {
    GrassCell {
        cell_id: 42,
        worldspace_id: 3,
        grid_x: 5,
        grid_y: 7,
        input_hash: [0; 32],
        heights: vec![120.0; 1089],
        normals: [0, 0, 127].repeat(1089),
        vertex_colors: vec![],
        water_height: None,
        quadrants: (0..4)
            .map(|quadrant| GrassQuadrant {
                quadrant,
                layers: layers.clone(),
                exclusion: vec![0; 512],
            })
            .collect(),
        obstruction_failures: vec![],
        expanded_sampling: 0,
    }
}

/// Author asymmetric ordered IDs independently of the sampler.
fn layer(ids: Vec<u32>, weight: f32, base: bool) -> GrassLayer {
    GrassLayer {
        texture_form_id: 81,
        is_base: base,
        grass_ids: ids,
        weights: vec![weight; 289],
    }
}

/// A fully dense unconstrained rule with a controllable grid spacing.
fn rule(id: u32, range: f32) -> GrassType {
    GrassType {
        form_id: id,
        model_path: format!("meshes/test/{id}.glb"),
        density: 100,
        min_slope: 0,
        max_slope: 90,
        units_from_water: 0,
        water_comparison: 0,
        position_range: range,
        height_range: 0.35,
        color_range: 0.15,
        wave_period: 2.5,
        flags: 4,
    }
}

/// Open the production mapped reader on purely synthetic serialized fields.
fn mapped(types: Vec<GrassType>, cell: GrassCell) -> GrassCache {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        file.path(),
        rkyv::to_bytes::<rkyv::rancor::Error>(&shared::grass::GrassCache {
            version: GRASS_CACHE_VERSION,
            types,
            cells: vec![cell],
        })
        .unwrap(),
    )
    .unwrap();
    GrassCache::open(file.path()).unwrap()
}

/// Per-spot admission distinguishes 25 from 26, binary support, base residual, and 16 entries.
#[test]
fn admission_uses_neighborhood_threshold_and_ordered_budget() {
    let mut layers = vec![
        layer(vec![9, 7, 11, 13], 0.001, true),
        layer(vec![17], 25.0 / 255.0, false),
        layer(vec![19], 0.0, false),
    ];
    layers[2].weights[0] = 26.0 / 255.0;
    for id in 20..36 {
        layers.push(layer(vec![id], 1.0, false));
    }
    let cache = mapped((7..36).map(|id| rule(id, 20.0)).collect(), cell(layers));
    let entries = entries(
        &cache.cell(42).unwrap().quadrants[0],
        1,
        1,
        &EngineConfig::default(),
    );
    assert_eq!(entries.len(), 16);
    assert_eq!(
        entries
            .iter()
            .take(5)
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        [9, 7, 11, 19, 20]
    );
    assert_eq!(
        entries[0].weights, [1.0; 9],
        "tiny base residual is full support"
    );
    assert_eq!(
        entries[3].weights,
        [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]
    );
}

/// Repeated forms roll independently and zero-support base entries still consume admission.
#[test]
fn duplicate_entries_and_zero_base_are_not_collapsed() {
    let cache = mapped(
        vec![rule(7, 20.0)],
        cell(vec![layer(vec![7], 0.0, true), layer(vec![7], 1.0, false)]),
    );
    let entries = entries(
        &cache.cell(42).unwrap().quadrants[0],
        1,
        1,
        &EngineConfig::default(),
    );
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].weights, [0.0; 9]);
    assert_eq!(entries[1].weights, [1.0; 9]);
    let single = generate(&cache, 42, &EngineConfig::default());
    assert_eq!(single.batches[0].instances.len(), 36864);
    let doubled = mapped(
        vec![rule(7, 20.0)],
        cell(vec![layer(vec![7], 1.0, true), layer(vec![7], 1.0, false)]),
    );
    assert_eq!(
        generate(&doubled, 42, &EngineConfig::default()).batches[0]
            .instances
            .len(),
        73728
    );
}

/// Position Range changes attempt counts rather than merely adding a fixed jitter span.
#[test]
fn position_range_controls_species_ratios_and_replay() {
    let cache = mapped(
        vec![rule(7, 20.0), rule(11, 40.0), rule(13, 257.0)],
        cell(vec![layer(vec![7, 11, 13], 1.0, true)]),
    );
    let config = EngineConfig::default();
    let generated = generate(&cache, 42, &config);
    assert_eq!(
        generated
            .batches
            .iter()
            .map(|batch| (batch.grass_id, batch.instances.len()))
            .collect::<Vec<_>>(),
        [(7, 36864), (11, 9216)]
    );
    let replay = generate(&cache, 42, &config);
    for (a, b) in generated.batches.iter().zip(&replay.batches) {
        assert_eq!(a.instances, b.instances);
        assert!(
            a.instances
                .windows(2)
                .all(|pair| pair[0].candidate_slot() <= pair[1].candidate_slot())
        );
        assert!(
            a.instances
                .iter()
                .all(|instance| instance.candidate_slot() < a.slot_count)
        );
    }
    assert_eq!(grid_side(0.0, 20.0), 12);
    assert_eq!(grid_side(-20.0, 20.0), 0);
    assert_eq!(grid_side(60.0, 20.0), 4);
}

/// The shared stream responds to entry order, while the coordinate product determines its seed.
#[test]
fn entry_order_advances_the_shared_stream() {
    let a = mapped(
        vec![rule(7, 20.0), rule(11, 20.0)],
        cell(vec![layer(vec![7, 11], 1.0, true)]),
    );
    let mut swapped = cell(vec![layer(vec![11, 7], 1.0, true)]);
    swapped.cell_id = 43;
    let b = mapped(vec![rule(7, 20.0), rule(11, 20.0)], swapped);
    let config = EngineConfig::default();
    assert_ne!(
        generate(&a, 42, &config).batches[0].instances,
        generate(&b, 43, &config).batches[0].instances
    );
    let mut same = cell(vec![layer(vec![7, 11], 1.0, true)]);
    same.cell_id = 44;
    let c = mapped(vec![rule(7, 20.0), rule(11, 20.0)], same);
    assert_eq!(
        generate(&a, 42, &config).batches[0].instances,
        generate(&c, 44, &config).batches[0].instances
    );
}

/// Final obstruction rejection preserves the stream used by surviving quadrants.
#[test]
fn exclusions_do_not_advance_or_reset_the_reference_stream() {
    let unmasked = mapped(vec![rule(7, 20.0)], cell(vec![layer(vec![7], 1.0, true)]));
    let mut blocked = cell(vec![layer(vec![7], 1.0, true)]);
    blocked.quadrants[0].exclusion.fill(255);
    let blocked = mapped(vec![rule(7, 20.0)], blocked);
    let config = EngineConfig::default();
    let baseline = generate(&unmasked, 42, &config);
    let generated = generate(&blocked, 42, &config);
    assert!((26000..29000).contains(&generated.batches[0].instances.len()));
    let allowed: Vec<_> = baseline.batches[0]
        .instances
        .iter()
        .filter(|instance| instance.position[0] > 32767 || instance.position[1] > 32767)
        .copied()
        .collect();
    assert_eq!(allowed, *generated.batches[0].instances);
}

/// Density zero and a disabled explicit multiplier emit nothing, without fabricated batches.
#[test]
fn zero_density_and_large_spacing_are_empty() {
    let mut zero = rule(7, 20.0);
    zero.density = 0;
    let cache = mapped(vec![zero], cell(vec![layer(vec![7], 1.0, true)]));
    assert!(
        generate(&cache, 42, &EngineConfig::default())
            .batches
            .is_empty()
    );
    let cache = mapped(vec![rule(7, 20.0)], cell(vec![layer(vec![7], 1.0, true)]));
    for config in [
        EngineConfig {
            grass_density_multiplier: 0.0,
            ..Default::default()
        },
        EngineConfig {
            grass_min_spacing: 257.0,
            ..Default::default()
        },
    ] {
        assert!(generate(&cache, 42, &config).batches.is_empty());
    }
}

/// Modes pin the sign of mode zero, inclusive boundaries, and the no-water behavior.
#[test]
fn water_boundaries_and_absent_water() {
    assert!(water_allowed(80.0, Some(100.0), 20, 0));
    assert!(!water_allowed(79.0, Some(100.0), 20, 0));
    for (mode, at_zero) in [
        (0, true),
        (1, true),
        (2, false),
        (3, true),
        (4, false),
        (5, true),
        (6, true),
        (7, true),
    ] {
        assert_eq!(water_allowed(100.0, Some(100.0), 20, mode), at_zero);
    }
    assert!(water_allowed(0.0, None, 20, 0));
    assert!(!water_allowed(0.0, None, 20, 2));
    assert!(water_allowed(0.0, None, 20, 6));
}

/// Literal color-range endpoints and white saturation distinguish the former wider law.
#[test]
fn brightness_uses_reference_amplitude_and_ceiling() {
    assert_eq!(brightness(1.0, 0.0, 0).0, 253);
    assert_eq!(brightness(1.0, 1.0, 0).0, 0);
    assert_eq!(brightness(1.0, 1.0, u32::MAX).0, 64);
    assert_eq!(brightness(0.37, 0.0, 0).0, 94);
    assert!(sampling::slope_allowed(0.8, 0, 40));
    assert!(!sampling::slope_allowed(0.8, 0, 30));
}

/// Repeated GNAM fields consume separate prefix places before a later species.
#[test]
fn repeated_gnam_occurrences_consume_the_prefix() {
    let cache = mapped(
        vec![rule(7, 20.0), rule(11, 20.0), rule(13, 20.0)],
        cell(vec![layer(vec![7, 7, 11, 13], 1.0, true)]),
    );
    let generated = generate(&cache, 42, &EngineConfig::default());
    assert_eq!(
        generated
            .batches
            .iter()
            .map(|batch| (batch.grass_id, batch.instances.len()))
            .collect::<Vec<_>>(),
        [(7, 73728), (11, 36864)]
    );
}

/// Distinct fitted and upright frames pin the reference's nonuniform angle distribution.
#[test]
fn orientation_angles_follow_the_reference_frame() {
    assert_eq!(rotation(0, false), 128);
    assert_eq!(rotation(1 << 31, false), 64);
    assert_eq!(rotation(u32::MAX, false), 0);
    assert_eq!(rotation(0, true), 64);
    assert_eq!(rotation(1 << 31, true), 0);
    assert_eq!(rotation(u32::MAX, true), 192);
}

/// Independent scalar-reference counts exercise failed density and conditional appearance rolls.
#[test]
fn sparse_density_and_rejected_land_filters_pin_stream_consumption() {
    let mut first = rule(7, 20.0);
    first.density = 30;
    let mut second = rule(11, 40.0);
    second.density = 70;
    let config = EngineConfig::default();
    for rejected in [0, 1, 2] {
        let mut first = first.clone();
        if rejected == 1 {
            first.water_comparison = 2;
        }
        if rejected == 2 {
            first.min_slope = 30;
        }
        let cache = mapped(
            vec![first, second.clone()],
            cell(vec![layer(vec![7, 11], 1.0, true)]),
        );
        let generated = generate(&cache, 42, &config);
        let counts: Vec<_> = generated
            .batches
            .iter()
            .map(|batch| (batch.grass_id, batch.instances.len()))
            .collect();
        let expected = if rejected == 0 {
            vec![(7, 10855), (11, 6450)]
        } else {
            vec![(11, 6449)]
        };
        assert_eq!(counts, expected, "filter case {rejected}");
    }
}

/// A committed surface with absent VCLR uses white, not old colours from its speculative cache.
#[test]
fn committed_absent_colours_do_not_reuse_cached_tint() {
    let mut source = cell(vec![layer(vec![7], 1.0, true)]);
    source.vertex_colors = vec![0; 3267];
    let mut species = rule(7, 20.0);
    species.color_range = 0.0;
    let cache = mapped(vec![species], source);
    let config = EngineConfig::default();
    assert!(
        generate(&cache, 42, &config).batches[0]
            .instances
            .iter()
            .all(|instance| instance.color == 0)
    );
    let surface = GrassTerrain::new(vec![120.0; 1089], None);
    assert!(
        generate_on_terrain(&cache, 42, &config, &surface).batches[0]
            .instances
            .iter()
            .all(|instance| instance.color == 253)
    );
}
