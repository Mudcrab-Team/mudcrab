//! LAND sampling behavior follows spud (MIT, chasmlol/spud), revision e5f2de4.
//! The full notice is retained in SPUD-MIT.txt. No game binaries were inspected.

pub(super) struct Sample {
    pub height: f32,
    pub normal: [f32; 3],
    pub nearest: usize,
}

/// Round to the reference's binary16 coordinate precision, including ties to even.
pub(super) fn half_round(value: f32) -> f32 {
    let magnitude = value.abs();
    if !value.is_finite() || magnitude >= 65_520.0 {
        return value.signum() * f32::INFINITY;
    }
    let exponent = ((magnitude.to_bits() >> 23) & 255) as i32 - 127;
    let step = 2.0f32.powi((exponent - 10).max(-24));
    (value / step).round_ties_even() * step
}

/// Locate an alternating LAND triangle; use its nearest vertex's authored normal.
pub(super) fn sample(
    x: f32,
    y: f32,
    height_at: &impl Fn(usize) -> f32,
    normal_at: &impl Fn(usize) -> Option<[f32; 3]>,
) -> Option<Sample> {
    if !(0.0..=4096.0).contains(&x) || !(0.0..=4096.0).contains(&y) {
        return None;
    }
    // Exact grid edges belong to the preceding square, as in the reference.
    let split = |value: f32| {
        let integer = value as usize;
        let index = (integer / 128)
            .saturating_sub(usize::from(integer != 0 && integer.is_multiple_of(128)))
            .min(31);
        (index, value - index as f32 * 128.0)
    };
    let (col, fx) = split(x);
    let (row, fy) = split(y);
    let odd = (col + row) & 1 != 0;
    let corners = if odd {
        [
            if fx + fy > 128.0 { [1, 1] } else { [0, 0] },
            [1, 0],
            [0, 1],
        ]
    } else {
        [if fx <= fy { [0, 1] } else { [1, 0] }, [0, 0], [1, 1]]
    };
    let index = |[c, r]: [usize; 2]| (row + r) * 33 + col + c;
    // Authored LAND uses the reference's eight-unit height reconstruction.
    // A synthetic/legacy surface without VNML uses its actual committed geometry
    // for both height and the geometric-normal fallback, including welded edges.
    let authored = normal_at(index(corners[0])).is_some();
    let heights = corners.map(|corner| {
        let height = height_at(index(corner));
        if authored {
            (height / 8.0).round() * 8.0
        } else {
            height
        }
    });
    let [a, b, c] = heights;
    let height = if odd {
        if fx + fy > 128.0 {
            a - ((a - b) * (128.0 - fy) + (a - c) * (128.0 - fx)) / 128.0
        } else {
            ((b - a) / 128.0 * fx + a) + (c - a) / 128.0 * fy
        }
    } else if fx <= fy {
        ((a - b) / 128.0 * fy + b) + (c - a) / 128.0 * fx
    } else {
        ((a - b) / 128.0 * fx + b) + (c - a) / 128.0 * fy
    };
    let mut nearest = index(corners[0]);
    let mut distance = f32::INFINITY;
    for corner in corners {
        let dx = fx - corner[0] as f32 * 128.0;
        let dy = fy - corner[1] as f32 * 128.0;
        let candidate = dx * dx + dy * dy;
        if candidate < distance {
            nearest = index(corner);
            distance = candidate;
        }
    }
    // Synthetic inputs may omit VNML. Fall back to this triangle's geometry.
    let normal = normal_at(nearest).unwrap_or_else(|| {
        let p = corners.map(|[cx, cy]| [cx as f32 * 128.0, cy as f32 * 128.0]);
        let u = [p[1][0] - p[0][0], p[1][1] - p[0][1], b - a];
        let v = [p[2][0] - p[0][0], p[2][1] - p[0][1], c - a];
        let n = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        n.map(|value| if n[2] < 0.0 { -value } else { value })
    });
    let length = normal.iter().map(|value| value * value).sum::<f32>().sqrt();
    let normal = normal.map(|value| if length > 0.0 { value / length } else { 0.0 });
    Some(Sample {
        height,
        normal,
        nearest,
    })
}

/// Compare the normalized authored normal with the reference's 512-sample cosine table.
pub(super) fn slope_allowed(normal_z: f32, min: u8, max: u8) -> bool {
    let cosine = |angle: u8| {
        let index = ((f32::from(angle) * 1.422_222_1) as usize) & 511;
        (std::f64::consts::TAU * index as f64 / 512.0).cos() as f32
    };
    cosine(max) <= normal_z && normal_z <= cosine(min)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Half precision ties round to even, including subnormals and negative blocks.
    #[test]
    fn coordinate_rounding() {
        for (input, expected) in [
            (2049.0, 2048.0),
            (2051.0, 2052.0),
            (-2051.0, -2052.0),
            (0.000_000_03, 2.0f32.powi(-24)),
        ] {
            assert_eq!(half_round(input), expected);
        }
    }

    /// A nonplanar even square distinguishes alternating triangles from a fixed diagonal.
    #[test]
    fn alternating_triangle_and_nearest_vertex() {
        let heights = |i| if i == 34 { 128.0 } else { 0.0 };
        let sampled = sample(96.0, 80.0, &heights, &|_| Some([0.0, 3.0, 4.0])).unwrap();
        assert_eq!(sampled.height, 80.0);
        assert_eq!(sampled.nearest, 34);
        assert_eq!(sampled.normal, [0.0, 0.6, 0.8]);
        assert!(sample(-0.1, 0.0, &heights, &|_| None).is_none());
    }
}
