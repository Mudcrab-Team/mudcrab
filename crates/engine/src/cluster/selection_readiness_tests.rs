// Included inside handoff::selection_regressions, which imports super::*.

fn readiness_test_key() -> ReadinessKey {
    ReadinessKey {
        materials: 1,
        material_events: 2,
        shader_invalidations: 3,
        generation: 4,
        layout: 5,
        batches: 2,
        view: 6,
        pipelines: 7,
        queued: 8,
        shadow_warmup: true,
        shadow: false,
        prepass: false,
    }
}

#[test]
fn equal_reevaluated_readiness_retains_arc_across_unrelated_epochs() {
    let key = readiness_test_key();
    let previous = (key, Arc::new(vec![true, false]));
    let mut all_epochs_changed = key;
    all_epochs_changed.materials += 1;
    all_epochs_changed.material_events += 1;
    all_epochs_changed.shader_invalidations += 1;
    all_epochs_changed.view += 1;
    all_epochs_changed.pipelines += 1;
    all_epochs_changed.queued += 1;

    for (epoch, current_key) in [
        ("unchanged", key),
        (
            "materials",
            ReadinessKey {
                materials: key.materials + 1,
                ..key
            },
        ),
        (
            "material events",
            ReadinessKey {
                material_events: key.material_events + 1,
                ..key
            },
        ),
        (
            "shader invalidations",
            ReadinessKey {
                shader_invalidations: key.shader_invalidations + 1,
                ..key
            },
        ),
        (
            "view",
            ReadinessKey {
                view: key.view + 1,
                ..key
            },
        ),
        (
            "pipelines",
            ReadinessKey {
                pipelines: key.pipelines + 1,
                ..key
            },
        ),
        (
            "queued proof",
            ReadinessKey {
                queued: key.queued + 1,
                ..key
            },
        ),
        ("all epochs", all_epochs_changed),
    ] {
        // This is a newly evaluated vector, not the cached Arc's allocation.
        let current = retain_equal_readiness(Some(&previous), current_key, vec![true, false]);
        assert_eq!(current.as_slice(), [true, false], "{epoch}");
        assert!(
            Arc::ptr_eq(&previous.1, &current),
            "equal bits after {epoch} must retain selection identity"
        );
    }
}

#[test]
fn changed_equal_length_readiness_requires_fresh_arc() {
    let key = readiness_test_key();
    let previous = (key, Arc::new(vec![true, false]));
    for bits in [vec![false, true], vec![false, false], vec![true, true]] {
        let current = retain_equal_readiness(Some(&previous), key, bits.clone());
        assert_eq!(current.as_ref(), &bits);
        assert!(
            !Arc::ptr_eq(&previous.1, &current),
            "changed bits {bits:?} must invalidate selection identity"
        );
        assert_eq!(previous.1.as_slice(), [true, false]);
    }
}

#[test]
fn absent_readiness_cache_starts_fresh_even_after_cache_withdrawal() {
    let key = readiness_test_key();
    let withdrawn = retain_equal_readiness(None, key, vec![true, false]);
    // A selection can still hold the old Arc after the cache is withdrawn.
    let recovered = retain_equal_readiness(None, key, vec![true, false]);
    assert_eq!(withdrawn.as_slice(), [true, false]);
    assert_eq!(recovered.as_slice(), [true, false]);
    assert!(!Arc::ptr_eq(&withdrawn, &recovered));
}

#[test]
fn readiness_withdrawal_and_recovery_each_require_fresh_arc() {
    let key = readiness_test_key();
    let ready = (key, Arc::new(vec![true, false]));
    let withdrawn_key = ReadinessKey {
        queued: key.queued + 1,
        ..key
    };
    let withdrawn = retain_equal_readiness(Some(&ready), withdrawn_key, vec![false, false]);
    assert_eq!(withdrawn.as_slice(), [false, false]);
    assert!(!Arc::ptr_eq(&ready.1, &withdrawn));

    let previous = (withdrawn_key, withdrawn);
    let recovered_key = ReadinessKey {
        queued: withdrawn_key.queued + 1,
        ..key
    };
    let recovered = retain_equal_readiness(Some(&previous), recovered_key, vec![true, false]);
    assert_eq!(recovered.as_slice(), [true, false]);
    assert!(!Arc::ptr_eq(&previous.1, &recovered));
    assert!(!Arc::ptr_eq(&ready.1, &recovered));
}

#[test]
fn gpu_generation_layout_and_batch_count_boundaries_require_fresh_arc_for_equal_bits() {
    let key = readiness_test_key();
    let previous = (key, Arc::new(vec![true, false]));
    for (boundary, current_key) in [
        (
            "generation",
            ReadinessKey {
                generation: key.generation + 1,
                ..key
            },
        ),
        (
            "layout",
            ReadinessKey {
                layout: key.layout + 1,
                ..key
            },
        ),
        (
            "batch count",
            ReadinessKey {
                batches: key.batches + 1,
                ..key
            },
        ),
    ] {
        let current = retain_equal_readiness(Some(&previous), current_key, vec![true, false]);
        assert_eq!(current.as_slice(), [true, false], "{boundary}");
        assert!(
            !Arc::ptr_eq(&previous.1, &current),
            "{boundary} must invalidate identity even for equal bits"
        );
    }
}

#[test]
fn shadow_and_prepass_role_boundaries_require_fresh_arc_for_equal_bits() {
    let key = readiness_test_key();
    let roles = [
        ("main", key),
        (
            "prepass",
            ReadinessKey {
                prepass: true,
                ..key
            },
        ),
        (
            "shadow",
            ReadinessKey {
                shadow: true,
                ..key
            },
        ),
    ];
    for (previous_role, previous_key) in roles {
        let previous = (previous_key, Arc::new(vec![true, false]));
        for (current_role, current_key) in roles {
            if previous_key == current_key {
                continue;
            }
            let current = retain_equal_readiness(Some(&previous), current_key, vec![true, false]);
            assert_eq!(current.as_slice(), [true, false]);
            // Selection's stale stamp omits the shadow role, so pointer identity
            // must force a rebuild when the same view changes its draw role.
            assert!(
                !Arc::ptr_eq(&previous.1, &current),
                "{previous_role} -> {current_role} must invalidate selection identity"
            );
        }
    }
}
