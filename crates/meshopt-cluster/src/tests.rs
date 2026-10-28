use super::*;

/// `n` by `n` quads with a small height pattern, two triangles per quad.
fn grid(n: usize) -> (Vec<[f32; 3]>, Vec<u32>) {
    let stride = n + 1;
    let mut positions = Vec::with_capacity(stride * stride);
    for y in 0..=n {
        for x in 0..=n {
            positions.push([x as f32, y as f32, ((x * 7 + y * 3) % 5) as f32 * 0.05]);
        }
    }

    let mut indices = Vec::with_capacity(n * n * 6);
    for y in 0..n {
        for x in 0..n {
            let a = (y * stride + x) as u32;
            let b = a + 1;
            let c = a + stride as u32;
            let d = c + 1;
            indices.extend_from_slice(&[a, c, b, b, c, d]);
        }
    }

    (positions, indices)
}

/// Triangles as sorted vertex triples, sorted: equal for equal triangle sets, whatever the order.
fn triangle_set(indices: &[u32]) -> Vec<[u32; 3]> {
    let mut triangles: Vec<[u32; 3]> = indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| {
            let mut v = [t[0], t[1], t[2]];
            v.sort();
            v
        })
        .collect();
    triangles.sort();
    triangles
}

fn build_ok(positions: &[[f32; 3]], indices: &[u32], config: Config) -> Hierarchy {
    build(
        &Mesh {
            positions,
            indices,
            attributes: None,
            vertex_lock: None,
        },
        config,
    )
    .expect("build")
}

/// Every source triangle appears exactly once among the clusters that no group produced.
fn assert_covers(h: &Hierarchy, indices: &[u32]) {
    let mut source = Vec::new();
    for c in h.clusters().iter().filter(|c| c.refined.is_none()) {
        source.extend_from_slice(c.indices());
    }
    assert_eq!(triangle_set(&source), triangle_set(indices));
}

#[test]
fn empty_index_list_gives_empty_hierarchy() {
    let positions = [[0.0f32; 3]; 3];
    let h = build_ok(&positions, &[], Config::classic(64));
    assert!(h.groups().is_empty());
    assert!(h.clusters().is_empty());
    assert_eq!(h.level_count(), 0);
    assert!(h.build_bvh(4).expect("bvh").nodes().is_empty());
}

#[test]
fn single_triangle_is_one_terminal_cluster() {
    let positions = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    let h = build_ok(&positions, &[0, 1, 2], Config::classic(64));

    assert_eq!(h.groups().len(), 1);
    assert_eq!(h.groups()[0].depth, 0);
    assert_eq!(h.groups()[0].bounds.error, f32::MAX);

    assert_eq!(h.clusters().len(), 1);
    let c = &h.clusters()[0];
    assert_eq!(c.refined, None);
    assert_eq!(c.triangle_count(), 1);
    assert_eq!(c.vertex_count, 3);
    let mut v = c.indices().to_vec();
    v.sort();
    assert_eq!(v, [0, 1, 2]);
}

#[test]
fn grid_clusters_cover_input_and_respect_max_triangles() {
    let (positions, indices) = grid(16);
    for config in [Config::classic(32), Config::rt(32)] {
        let h = build_ok(&positions, &indices, config);
        assert_covers(&h, &indices);
        for c in h.clusters() {
            assert!(
                (1..=32).contains(&c.triangle_count()),
                "cluster has {} triangles",
                c.triangle_count()
            );
        }
    }
}

#[test]
fn dag_is_consistent() {
    let (positions, indices) = grid(16);
    let h = build_ok(&positions, &indices, Config::classic(32));

    // each cluster belongs to exactly one group; the group ranges tile the cluster list
    let mut owner = vec![usize::MAX; h.clusters().len()];
    for (gi, g) in h.groups().iter().enumerate() {
        for ci in g.clusters.clone() {
            assert_eq!(owner[ci], usize::MAX, "cluster {ci} is in two groups");
            owner[ci] = gi;
        }
    }
    assert!(owner.iter().all(|&g| g != usize::MAX));

    for (ci, c) in h.clusters().iter().enumerate() {
        let gi = owner[ci];
        match c.refined {
            None => assert_eq!(h.groups()[gi].depth, 0),
            Some(parent) => {
                assert!(
                    parent < gi,
                    "group {gi} consumes a cluster from later group {parent}"
                );
                assert_eq!(h.groups()[gi].depth, h.groups()[parent].depth + 1);
            }
        }
    }

    assert!(
        h.groups().windows(2).all(|w| w[0].depth <= w[1].depth),
        "groups are not in level order"
    );
    let last = h.groups().last().expect("at least one group");
    assert_eq!(last.depth as usize + 1, h.level_count());
    assert_eq!(last.bounds.error, f32::MAX, "final group must be terminal");
    for g in &h.groups()[..h.groups().len() - 1] {
        assert!(g.bounds.error.is_finite());
    }
}

#[test]
fn invalid_input_is_rejected() {
    let positions = [[0.0f32; 3]; 3];
    let mesh = |indices: &'static [u32]| Mesh {
        positions: &positions,
        indices,
        attributes: None,
        vertex_lock: None,
    };

    assert_eq!(
        build(&mesh(&[0, 1]), Config::classic(64)).unwrap_err(),
        Error::IndexCount(2)
    );
    assert_eq!(
        build(&mesh(&[0, 1, 3]), Config::classic(64)).unwrap_err(),
        Error::IndexOutOfRange {
            index: 3,
            vertex_count: 3
        }
    );
    assert_eq!(
        build(&mesh(&[0, 1, 2]), Config::classic(3)).unwrap_err(),
        Error::MaxTriangles(3)
    );
    assert_eq!(
        build(&mesh(&[0, 1, 2]), Config::rt(257)).unwrap_err(),
        Error::MaxTriangles(257)
    );

    let nan = [[f32::NAN, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    let nan_mesh = Mesh {
        positions: &nan,
        indices: &[0, 1, 2],
        attributes: None,
        vertex_lock: None,
    };
    assert_eq!(
        build(&nan_mesh, Config::classic(64)).unwrap_err(),
        Error::NonFinitePosition { vertex: 0 }
    );

    let short = [0.0f32; 5];
    let attrs = Attributes {
        data: &short,
        stride: 2,
        weights: &[1.0],
        protect_mask: 0,
    };
    let bad_len = Mesh {
        positions: &positions,
        indices: &[0, 1, 2],
        attributes: Some(attrs),
        vertex_lock: None,
    };
    assert_eq!(
        build(&bad_len, Config::classic(64)).unwrap_err(),
        Error::AttributeDataLength {
            expected: 6,
            actual: 5
        }
    );

    let one = [0.0f32; 3];
    let attrs = Attributes {
        data: &one,
        stride: 1,
        weights: &[1.0, 1.0],
        protect_mask: 0,
    };
    let too_many = Mesh {
        positions: &positions,
        indices: &[0, 1, 2],
        attributes: Some(attrs),
        vertex_lock: None,
    };
    assert_eq!(
        build(&too_many, Config::classic(64)).unwrap_err(),
        Error::AttributeWeights {
            stride: 1,
            count: 2
        }
    );

    let six = [0.0f32; 6];
    let attrs = Attributes {
        data: &six,
        stride: 2,
        weights: &[1.0],
        protect_mask: 4,
    };
    let mask = Mesh {
        positions: &positions,
        indices: &[0, 1, 2],
        attributes: Some(attrs),
        vertex_lock: None,
    };
    assert_eq!(
        build(&mask, Config::classic(64)).unwrap_err(),
        Error::AttributeProtectMask { stride: 2, mask: 4 }
    );

    let h = build_ok(&positions, &[0, 1, 2], Config::classic(64));
    assert_eq!(h.build_bvh(1).unwrap_err(), Error::NodeWidth(1));
}

#[test]
fn attributes_are_accepted_with_and_without_seam_mask() {
    let (positions, indices) = grid(8);
    let uv: Vec<f32> = positions
        .iter()
        .flat_map(|p| [p[0] / 8.0, p[1] / 8.0])
        .collect();
    for protect_mask in [0u32, 1, 2] {
        let attrs = Attributes {
            data: &uv,
            stride: 2,
            weights: &[1.0, 1.0],
            protect_mask,
        };
        let mesh = Mesh {
            positions: &positions,
            indices: &indices,
            attributes: Some(attrs),
            vertex_lock: None,
        };
        let h = build(&mesh, Config::classic(32)).expect("attribute build");
        assert_covers(&h, &indices);
    }
}

#[test]
fn local_geometry_round_trips() {
    let (positions, indices) = grid(8);
    let h = build_ok(&positions, &indices, Config::classic(32));
    for c in h.clusters() {
        let local = c.local_geometry();
        assert_eq!(local.triangles.len(), c.indices().len());
        assert_eq!(local.vertices.len(), c.vertex_count);
        for (i, &t) in local.triangles.iter().enumerate() {
            assert_eq!(local.vertices[t as usize], c.indices()[i]);
        }
    }
}

#[test]
fn bvh_leaves_hold_each_group_once() {
    let (positions, indices) = grid(16);
    let h = build_ok(&positions, &indices, Config::rt(32));
    let bvh = h.build_bvh(4).expect("bvh");

    assert_eq!(bvh.roots().len(), h.level_count());
    let mut leaves: Vec<usize> = bvh.nodes().iter().filter_map(|n| n.group).collect();
    leaves.sort();
    assert_eq!(leaves, (0..h.groups().len()).collect::<Vec<_>>());

    for n in bvh.nodes() {
        assert!(n.children.end <= bvh.nodes().len());
        if n.group.is_some() {
            assert!(n.children.is_empty());
        } else {
            assert!((1..=4).contains(&n.children.len()));
        }
    }
}

#[test]
fn build_is_deterministic() {
    let (positions, indices) = grid(12);
    let a = build_ok(&positions, &indices, Config::classic(32));
    let b = build_ok(&positions, &indices, Config::classic(32));
    assert_eq!(a, b);
}

#[test]
fn tail_levels_coarsen_the_root_without_breaking_the_dag() {
    let (positions, indices) = grid(16);
    let plain = build_ok(&positions, &indices, Config::classic(32));
    let tail = build_ok(&positions, &indices, Config::classic(32).with_tail(4));

    // the tail adds groups and clusters on top; everything below it is the plain build's
    assert!(tail.groups().len() > plain.groups().len());
    assert_covers(&tail, &indices);

    // one terminal group, a single cluster of at most the tail size
    let terminal: Vec<_> = tail
        .groups()
        .iter()
        .filter(|g| g.bounds.error == f32::MAX)
        .collect();
    assert_eq!(terminal.len(), 1);
    let root = &tail.clusters()[terminal[0].clusters.start];
    assert!(
        root.triangle_count() <= 8,
        "root has {}",
        root.triangle_count()
    );

    // errors never fall going up the DAG, and every cluster's group exists before its consumer
    for (gi, g) in tail.groups().iter().enumerate() {
        for ci in g.clusters.clone() {
            if let Some(parent) = tail.clusters()[ci].refined {
                assert!(parent < gi);
                assert!(
                    g.bounds.error >= tail.groups()[parent].bounds.error,
                    "group {gi} error {} below its producer's {}",
                    g.bounds.error,
                    tail.groups()[parent].bounds.error
                );
                assert_eq!(g.depth, tail.groups()[parent].depth + 1);
            }
        }
    }
}

/// No cluster is empty and no group is without clusters, tail levels included.
///
/// The tail's simplifier call cannot be made to return zero indices (clusterlod replaces an empty
/// result with the first input triangle), so the guard in the wrapper is unreachable through
/// input; this checks the invariant it protects over fixtures that drive the tail hardest:
/// the smallest allowed tail size, a fully locked mesh where simplification is stuck, and a mesh
/// whose vertices all coincide so every simplification collapses.
#[test]
fn tail_levels_never_emit_empty_clusters_or_groups() {
    let assert_non_empty = |h: &Hierarchy, what: &str| {
        for (gi, g) in h.groups().iter().enumerate() {
            assert!(!g.clusters.is_empty(), "{what}: group {gi} has no clusters");
        }
        for (ci, c) in h.clusters().iter().enumerate() {
            assert!(c.triangle_count() > 0, "{what}: cluster {ci} is empty");
            assert!(c.vertex_count > 0, "{what}: cluster {ci} has no vertices");
        }
    };

    let (positions, indices) = grid(16);
    let locked = vec![1u8; positions.len()];
    let collapsed = vec![[0.5f32; 3]; positions.len()];

    for tail in [1, 2, 4, 8, 32] {
        let config = Config::classic(32).with_tail(tail);

        let free = build_ok(&positions, &indices, config);
        assert_non_empty(&free, "free grid");
        assert_covers(&free, &indices);

        let stuck = build(
            &Mesh {
                positions: &positions,
                indices: &indices,
                attributes: None,
                vertex_lock: Some(&locked),
            },
            config,
        )
        .expect("locked build");
        assert_non_empty(&stuck, "fully locked grid");

        let flat = build_ok(&collapsed, &indices, config);
        assert_non_empty(&flat, "coincident vertices");
    }
}

/// Vertices on the outer ring of an `n` by `n` quad grid.
fn border_vertices(n: usize) -> Vec<u32> {
    let stride = n + 1;
    (0..stride * stride)
        .filter(|&v| {
            let (x, y) = (v % stride, v / stride);
            x == 0 || y == 0 || x == n || y == n
        })
        .map(|v| v as u32)
        .collect()
}

/// The border vertices that some cluster of a terminal group (the coarsest level) still uses.
fn border_kept_at_root(h: &Hierarchy, border: &[u32]) -> usize {
    let mut used = std::collections::HashSet::new();
    for group in h.groups().iter().filter(|g| g.bounds.error == f32::MAX) {
        for cluster in h.group_clusters(group) {
            used.extend(cluster.indices().iter().copied());
        }
    }
    border.iter().filter(|v| used.contains(v)).count()
}

#[test]
fn locked_border_survives_every_level_including_the_tail() {
    let n = 64;
    let (positions, indices) = grid(n);
    let border = border_vertices(n);
    let mut lock = vec![0u8; positions.len()];
    for &v in &border {
        lock[v as usize] = 1;
    }
    let config = Config::classic(64).with_tail(8);

    let free = build_ok(&positions, &indices, config);
    assert!(
        border_kept_at_root(&free, &border) < border.len(),
        "without locks the border is expected to simplify away (the test would prove nothing)"
    );

    let locked = build(
        &Mesh {
            positions: &positions,
            indices: &indices,
            attributes: None,
            vertex_lock: Some(&lock),
        },
        config,
    )
    .expect("build");
    assert_covers(&locked, &indices);
    assert_eq!(border_kept_at_root(&locked, &border), border.len());
}

#[test]
fn vertex_lock_length_is_checked() {
    let (positions, indices) = grid(4);
    let lock = vec![0u8; positions.len() - 1];
    let mesh = Mesh {
        positions: &positions,
        indices: &indices,
        attributes: None,
        vertex_lock: Some(&lock),
    };
    assert_eq!(
        build(&mesh, Config::classic(64)).unwrap_err(),
        Error::VertexLockLength {
            expected: positions.len(),
            actual: positions.len() - 1
        }
    );
}

#[test]
fn attribute_count_above_native_limit_is_rejected() {
    let (positions, indices) = grid(8);
    let data = vec![0.0; positions.len() * 33];
    for weight in [0.0, 1.0] {
        let weights = [weight; 33];
        let mesh = Mesh {
            positions: &positions,
            indices: &indices,
            attributes: Some(Attributes {
                data: &data,
                stride: 33,
                weights: &weights,
                protect_mask: 0,
            }),
            vertex_lock: None,
        };
        assert_eq!(
            build(&mesh, Config::classic(32)).unwrap_err(),
            Error::AttributeWeights {
                stride: 33,
                count: 33,
            }
        );
    }
}

#[test]
fn invalid_attribute_weights_are_rejected() {
    let (positions, indices) = grid(8);
    let data = vec![0.0; positions.len() * 32];
    for weight in [-1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut weights = [1.0; 32];
        weights[31] = weight;
        let mesh = Mesh {
            positions: &positions,
            indices: &indices,
            attributes: Some(Attributes {
                data: &data,
                stride: 32,
                weights: &weights,
                protect_mask: 0,
            }),
            vertex_lock: None,
        };
        assert_eq!(
            build(&mesh, Config::classic(32)).unwrap_err(),
            Error::AttributeWeight { attribute: 31 }
        );
    }
}

#[test]
fn attribute_stride_above_native_limit_is_rejected() {
    let (positions, indices) = grid(8);
    let data = vec![0.0; positions.len() * 65];
    let mesh = Mesh {
        positions: &positions,
        indices: &indices,
        attributes: Some(Attributes {
            data: &data,
            stride: 65,
            weights: &[1.0],
            protect_mask: 0,
        }),
        vertex_lock: None,
    };
    assert_eq!(
        build(&mesh, Config::classic(32)).unwrap_err(),
        Error::AttributeStride(65)
    );
}

#[test]
fn attribute_size_arithmetic_overflow_is_rejected() {
    let positions = [[0.0; 3]; 3];
    for (positions, stride, expected) in [
        (
            &positions[..],
            usize::MAX,
            Error::AttributeSizeOverflow {
                vertex_count: 3,
                stride: usize::MAX,
            },
        ),
        (
            &positions[..1],
            usize::MAX / size_of::<f32>() + 1,
            Error::AttributeStride(usize::MAX / size_of::<f32>() + 1),
        ),
    ] {
        let mesh = Mesh {
            positions,
            indices: &[],
            attributes: Some(Attributes {
                data: &[],
                stride,
                weights: &[],
                protect_mask: 0,
            }),
            vertex_lock: None,
        };
        assert_eq!(build(&mesh, Config::classic(32)).unwrap_err(), expected);
    }
}

#[test]
fn maximum_attribute_count_and_stride_are_accepted() {
    let (mut positions, mut indices) = grid(8);
    // A duplicate position makes clodBuild inspect seam bits, including bit 31.
    let duplicate = positions.len();
    positions.push(positions[40]);
    *indices.iter_mut().find(|index| **index == 40).unwrap() = duplicate as u32;
    for stride in [32, 33, 64] {
        let mut data = vec![0.0; positions.len() * stride];
        data[duplicate * stride + 31] = 0.001;
        for protect_mask in [0, 1u32 << 31, u32::MAX] {
            for config in [Config::classic(32), Config::rt(32)] {
                let mesh = Mesh {
                    positions: &positions,
                    indices: &indices,
                    attributes: Some(Attributes {
                        data: &data,
                        stride,
                        weights: &[1.0; 32],
                        protect_mask,
                    }),
                    vertex_lock: None,
                };
                let h = build(&mesh, config.with_tail(4)).expect("maximum-width attribute build");
                assert_covers(&h, &indices);
                assert!(h.level_count() > 1, "must exercise native simplification");
            }
        }
    }
}

#[test]
fn zero_and_negative_zero_attribute_weights_are_accepted() {
    let (positions, indices) = grid(8);
    let data = vec![0.0; positions.len() * 2];
    let mesh = Mesh {
        positions: &positions,
        indices: &indices,
        attributes: Some(Attributes {
            data: &data,
            stride: 2,
            weights: &[0.0, -0.0],
            protect_mask: 0,
        }),
        vertex_lock: None,
    };
    let h = build(&mesh, Config::classic(32)).expect("zero weights are valid");
    assert_covers(&h, &indices);
}

#[test]
fn zero_vertex_mesh_gives_empty_hierarchy() {
    for config in [Config::classic(32), Config::rt(32)] {
        for attributes in [
            None,
            Some(Attributes {
                data: &[],
                stride: 32,
                weights: &[1.0; 32],
                protect_mask: u32::MAX,
            }),
        ] {
            let mesh = Mesh {
                positions: &[],
                indices: &[],
                attributes,
                vertex_lock: Some(&[]),
            };
            let hierarchy = build(&mesh, config.with_tail(4)).expect("empty mesh");
            assert!(hierarchy.groups().is_empty());
            assert!(hierarchy.clusters().is_empty());
            assert_eq!(hierarchy.level_count(), 0);
            assert!(
                hierarchy
                    .build_bvh(4)
                    .expect("empty bvh")
                    .nodes()
                    .is_empty()
            );
        }
    }
}

#[test]
fn empty_mesh_still_validates_all_inputs() {
    let mut mesh = Mesh {
        positions: &[],
        indices: &[],
        attributes: None,
        vertex_lock: None,
    };
    assert_eq!(
        build(&mesh, Config::classic(3)),
        Err(Error::MaxTriangles(3))
    );
    mesh.indices = &[0, 0, 0];
    assert_eq!(
        build(&mesh, Config::classic(32)),
        Err(Error::IndexOutOfRange {
            index: 0,
            vertex_count: 0
        })
    );
    mesh.indices = &[];
    mesh.vertex_lock = Some(&[0]);
    assert_eq!(
        build(&mesh, Config::classic(32)),
        Err(Error::VertexLockLength {
            expected: 0,
            actual: 1
        })
    );
    mesh.vertex_lock = None;
    mesh.attributes = Some(Attributes {
        data: &[],
        stride: 0,
        weights: &[],
        protect_mask: 1,
    });
    assert_eq!(
        build(&mesh, Config::classic(32)),
        Err(Error::AttributeProtectMask { stride: 0, mask: 1 })
    );
}
