//! The pure parts of retail's HDR chain (`retail_hdr.rs`): the target sizes, the pass order, the
//! bloom blur tables and the frame time that drives eye adaptation. Facts: skyrim-research
//! NEEDS 27 (reviewed), with NEEDS 18 D4 for the tap tables.

/// The raw frame time above which the engine's timer substitutes 1/6 s (NEEDS 27).
pub const MAX_FRAME_SECONDS: f32 = 1.0 / 6.0;
/// The history of the timer, `iUnstableFrameTimeHistorySize` (compiled default 8).
const HISTORY: usize = 8;
/// STAND-IN(#1114): bound frame-history storage and per-frame scan work.
pub(crate) const MAX_FRAME_TIME_HISTORY: usize = 256;
/// The value the timer's history starts with (NEEDS 27).
const HISTORY_PREFILL: f32 = 0.016;
/// The cap on the largest sample that the trimmed mean removes (NEEDS 27).
const REMOVED_MAXIMUM_CAP: f32 = 0.05;

/// The smoothed real frame time that drives the adaptation (`T`): with the last 8 frame times,
/// `(sum - min(largest, 0.05) - smallest) / 6`, so a spike above 0.05 s is only partly removed.
/// A measured time above 1/6 s counts as 1/6 s. The global time multiplier is 1.0 in the image.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameTimer {
    history: Vec<f32>,
    capacity: usize,
    next: usize,
}

impl Default for FrameTimer {
    fn default() -> Self {
        Self {
            history: Vec::new(),
            capacity: HISTORY,
            next: 0,
        }
    }
}

impl FrameTimer {
    pub fn adaptation_time(
        &mut self,
        seconds: f32,
        multiplier: f32,
        paused: bool,
        config: &super::retail_hdr::RetailHdrConfig,
    ) -> f32 {
        let capacity = config
            .frame_time_history_size
            .clamp(3, MAX_FRAME_TIME_HISTORY);
        if self.capacity != capacity {
            self.history.clear();
            self.capacity = capacity;
            self.next = 0;
        }
        self.record(seconds);
        if paused {
            return config.menu_adapt_timer;
        }
        let step = match config.fps_clamp {
            1..=199 => 1.0 / f32::from(config.fps_clamp),
            _ if config.compensate_unstable_frame_time => self.trimmed_mean(),
            _ => frame_sample(seconds),
        };
        step * multiplier
    }

    /// Records the time of the frame that just ended and returns `T`. A value that is not finite
    /// or is negative counts as 0.
    #[cfg(test)]
    pub fn push(&mut self, seconds: f32) -> f32 {
        self.record(seconds);
        self.trimmed_mean()
    }

    fn record(&mut self, seconds: f32) {
        let seconds = frame_sample(seconds);
        if self.next == self.history.len() {
            self.history.push(seconds);
        } else {
            self.history[self.next] = seconds;
        }
        self.next = (self.next + 1) % self.capacity;
    }

    fn trimmed_mean(&self) -> f32 {
        let missing = self.capacity - self.history.len();
        let mut largest = if missing > 0 { HISTORY_PREFILL } else { 0.0 };
        let mut smallest = if missing > 0 {
            HISTORY_PREFILL
        } else {
            f32::INFINITY
        };
        let mut sum = f64::from(HISTORY_PREFILL) * count_as_f64(missing);
        for &sample in &self.history {
            largest = largest.max(sample);
            smallest = smallest.min(sample);
            sum += f64::from(sample);
        }
        let mean = (sum - f64::from(largest.min(REMOVED_MAXIMUM_CAP)) - f64::from(smallest))
            / count_as_f64(self.capacity - 2);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the mean is bounded by the sample range"
        )]
        let mean = mean as f32;
        mean.max(0.0)
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "sample counts are used for a floating-point mean"
)]
fn count_as_f64(count: usize) -> f64 {
    count as f64
}

fn frame_sample(seconds: f32) -> f32 {
    if seconds.is_finite() {
        seconds.clamp(0.0, MAX_FRAME_SECONDS)
    } else {
        0.0
    }
}

/// The blur weights of each bloom radius `k` (1 to 7), for the taps `u = 0, 1, .. k`. The weight of
/// `-u` equals that of `u`. Engine constants as stored, read from the executable (NEEDS 27).
#[allow(
    clippy::excessive_precision,
    reason = "the stored engine constants, kept as the finding prints them"
)]
pub const BLOOM_WEIGHTS: [[f32; 8]; 7] = [
    [0.786985755, 0.10650672, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    [
        0.402619928,
        0.244201481,
        0.054488569,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
    ],
    [
        0.270682156,
        0.216745213,
        0.11128068,
        0.036632769,
        0.0,
        0.0,
        0.0,
        0.0,
    ],
    [
        0.204163671,
        0.180173829,
        0.123831593,
        0.066282138,
        0.027630489,
        0.0,
        0.0,
        0.0,
    ],
    [
        0.163967222,
        0.151360765,
        0.119064637,
        0.079811372,
        0.045588955,
        0.022190502,
        0.0,
        0.0,
    ],
    [
        0.137022808,
        0.129617959,
        0.109719232,
        0.083108582,
        0.05633172,
        0.03416691,
        0.018543981,
        0.0,
    ],
    [
        0.117695801,
        0.112988554,
        0.099966809,
        0.081512466,
        0.061254777,
        0.042423215,
        0.027077802,
        0.015928362,
    ],
];

/// The bloom radius: `k = clamp(trunc(BloomBlurRadius), 1, 7)`. The fraction is not used.
pub fn bloom_radius(blur_radius: f32) -> usize {
    1 + (2..=7_u8)
        .take_while(|k| blur_radius >= f32::from(*k))
        .count()
}

/// The weight of tap `u` (`-7..=7`) for radius `k`: entry `u + 7` of the shader's array.
pub fn bloom_taps(k: usize) -> [f32; 15] {
    let table = BLOOM_WEIGHTS[k.clamp(1, 7) - 1];
    std::array::from_fn(|entry| table[entry.abs_diff(7)])
}

/// The sizes of the luminance chain, from the first downsample to the adapted pair. The first
/// target is `max(ceil(W / 4), 2)` by `max(ceil(H / 4), 2)`, and each next one divides again by 4
/// with the same minimum, until the size is 2 by 2. With `bUseMultipleLuminanceReferences` 0 the
/// chain goes on to a 1 by 1 target. The last entry is the adapted pair's size.
pub fn chain_sizes(width: u32, height: u32, multiple_references: bool) -> Vec<(u32, u32)> {
    let quarter = |(w, h): (u32, u32)| (w.div_ceil(4).max(2), h.div_ceil(4).max(2));
    let mut sizes = vec![quarter((width, height))];
    loop {
        let last = sizes[sizes.len() - 1];
        // The adapted pair is its own target, even when the first downsample is already 2 by 2.
        if last == (2, 2) && sizes.len() > 1 {
            break;
        }
        sizes.push(quarter(last));
    }
    if !multiple_references {
        sizes.push((1, 1));
    }
    sizes
}

/// The size of the bloom and blur-swap targets: `W >> 2` by `H >> 2`, at least 1.
pub fn bloom_size(width: u32, height: u32) -> (u32, u32) {
    ((width >> 2).max(1), (height >> 2).max(1))
}

/// A texture of the chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChainTarget {
    /// The scene the tonemap reads.
    Scene,
    /// Intermediate level `i` of the luminance chain. Level 0 is `kHDR_DOWNSAMPLE0`, in colour.
    Level(usize),
    /// One of the two adapted-pair textures, which alternate each frame.
    Adapted(usize),
    /// `kHDR_BLURSWAP`.
    BloomSwap,
    /// `kHDR_BLOOM`.
    Bloom,
}

/// The shader entry point of a pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PassKind {
    Downsample4,
    Luminance4,
    Luminance16,
    Adapt,
    AdaptColour,
    BrightBlurVertical,
    BlurHorizontal,
}

impl PassKind {
    pub const ALL: [Self; 7] = [
        Self::Downsample4,
        Self::Luminance4,
        Self::Luminance16,
        Self::Adapt,
        Self::AdaptColour,
        Self::BrightBlurVertical,
        Self::BlurHorizontal,
    ];

    /// The position of the kind in [`Self::ALL`], which is the order of the pipelines.
    pub fn slot(self) -> usize {
        match self {
            Self::Downsample4 => 0,
            Self::Luminance4 => 1,
            Self::Luminance16 => 2,
            Self::Adapt => 3,
            Self::AdaptColour => 4,
            Self::BrightBlurVertical => 5,
            Self::BlurHorizontal => 6,
        }
    }

    pub fn entry_point(self) -> &'static str {
        match self {
            Self::Downsample4 => "downsample4",
            Self::Luminance4 => "luminance4",
            Self::Luminance16 => "luminance16",
            Self::Adapt => "adapt",
            Self::AdaptColour => "adapt_colour",
            Self::BrightBlurVertical => "bright_blur_vertical",
            Self::BlurHorizontal => "blur_horizontal",
        }
    }
}

/// One full-screen pass: read `source` (and `previous` for the adapt pass), write `target`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainPass {
    pub kind: PassKind,
    pub source: ChainTarget,
    pub previous: ChainTarget,
    pub target: ChainTarget,
}

/// The passes of one frame, in order. `levels` is the length of [`chain_sizes`]. `current` is the
/// adapted texture that holds the last frame's pair, and `adapted_valid` says it holds one. The
/// new pair is written to the other texture.
///
/// With a pair, the adapt draw replaces the last luminance step. Without one (the first frame, or
/// after the targets were rebuilt, which releases the pair) the last step is drawn as usual and
/// becomes the pair, so `A.x = A.y` and the tonemap ratio is exactly 1 (NEEDS 27). The bloom reads
/// the first downsample.
pub fn chain_plan(levels: usize, adapted_valid: bool, current: usize) -> Vec<ChainPass> {
    let intermediates = levels.saturating_sub(1).max(1);
    let previous = ChainTarget::Adapted(current);
    let pass = |kind, source, target| ChainPass {
        kind,
        source,
        previous,
        target,
    };
    let mut passes = vec![pass(
        PassKind::Downsample4,
        ChainTarget::Scene,
        ChainTarget::Level(0),
    )];
    for level in 1..intermediates {
        let kind = if level == 1 {
            PassKind::Luminance4
        } else {
            PassKind::Luminance16
        };
        passes.push(pass(
            kind,
            ChainTarget::Level(level - 1),
            ChainTarget::Level(level),
        ));
    }
    let last_kind = match (adapted_valid, intermediates == 1) {
        (true, true) => PassKind::AdaptColour,
        (true, false) => PassKind::Adapt,
        (false, true) => PassKind::Luminance4,
        (false, false) => PassKind::Luminance16,
    };
    passes.push(pass(
        last_kind,
        ChainTarget::Level(intermediates - 1),
        ChainTarget::Adapted(1 - current),
    ));
    passes.push(pass(
        PassKind::BrightBlurVertical,
        ChainTarget::Level(0),
        ChainTarget::BloomSwap,
    ));
    passes.push(pass(
        PassKind::BlurHorizontal,
        ChainTarget::BloomSwap,
        ChainTarget::Bloom,
    ));
    passes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(actual: f32, expected: f32, what: &str) {
        assert!(
            (actual - expected).abs() < 1.0e-6,
            "{what}: expected {expected}, got {actual}"
        );
    }

    /// NEEDS 27: for 1920 by 1080 the sizes are 480x270, 120x68, 30x17, 8x5 and 2x2.
    #[test]
    fn the_luminance_chain_ends_at_two_by_two() {
        assert_eq!(
            chain_sizes(1920, 1080, true),
            vec![(480, 270), (120, 68), (30, 17), (8, 5), (2, 2)]
        );
        assert_eq!(
            chain_sizes(1280, 720, true),
            vec![(320, 180), (80, 45), (20, 12), (5, 3), (2, 2)]
        );
        assert_eq!(chain_sizes(1920, 1080, false).last(), Some(&(1, 1)));
        // A tiny window still has a separate pair target.
        assert_eq!(chain_sizes(4, 4, true), vec![(2, 2), (2, 2)]);
        assert_eq!(bloom_size(1920, 1080), (480, 270));
        // `W >> 2`, not the ceiling of the first downsample.
        assert_eq!(bloom_size(1283, 723), (320, 180));
        assert_eq!(bloom_size(1, 1), (1, 1));
    }

    /// The order of the passes: the adapt draw replaces the last luminance step when a pair
    /// exists, and the first frame draws the step into the pair.
    #[test]
    fn the_adapt_draw_replaces_the_last_luminance_step() {
        let first = chain_plan(5, false, 0);
        let with_pair = chain_plan(5, true, 0);
        let kinds = |plan: &[ChainPass]| plan.iter().map(|pass| pass.kind).collect::<Vec<_>>();
        use PassKind::{
            Adapt, AdaptColour, BlurHorizontal, BrightBlurVertical, Downsample4, Luminance4,
            Luminance16,
        };
        assert_eq!(
            kinds(&first),
            vec![
                Downsample4,
                Luminance4,
                Luminance16,
                Luminance16,
                Luminance16,
                BrightBlurVertical,
                BlurHorizontal
            ]
        );
        assert_eq!(
            kinds(&with_pair),
            vec![
                Downsample4,
                Luminance4,
                Luminance16,
                Luminance16,
                Adapt,
                BrightBlurVertical,
                BlurHorizontal
            ]
        );
        let adapt = with_pair[4];
        assert_eq!(adapt.source, ChainTarget::Level(3));
        assert_eq!(adapt.previous, ChainTarget::Adapted(0));
        assert_eq!(adapt.target, ChainTarget::Adapted(1));
        // The pair alternates, so no pass reads the target it writes.
        for pass in chain_plan(5, true, 1) {
            assert_ne!(pass.source, pass.target);
            assert_ne!(pass.previous, pass.target);
        }
        // The bloom reads the first downsample.
        assert_eq!(with_pair[5].source, ChainTarget::Level(0));
        assert_eq!(with_pair[6].target, ChainTarget::Bloom);
        // A chain with one intermediate draws the first luminance step into the pair.
        assert_eq!(chain_plan(2, false, 0)[1].kind, Luminance4);
        assert_eq!(chain_plan(2, true, 0)[1].kind, AdaptColour);
    }

    #[test]
    fn each_pass_kind_sits_in_its_slot() {
        for (slot, kind) in PassKind::ALL.into_iter().enumerate() {
            assert_eq!(kind.slot(), slot);
        }
    }

    /// The weight tables of NEEDS 27, pinned: symmetric, zero beyond the radius, sum 1.
    #[test]
    fn the_bloom_weights_are_the_stored_tables() {
        let one = bloom_taps(1);
        close(one[6], 0.106_506_72, "k 1, u -1");
        close(one[7], 0.786_985_75, "k 1, u 0");
        close(one[8], 0.106_506_72, "k 1, u 1");
        assert_eq!(one[5], 0.0);
        // Radius 1 as the finding prints it.
        close(one[6], 0.106_506_7, "printed");
        close(one[7], 0.786_985_8, "printed");
        let three = bloom_taps(3);
        for (entry, expected) in [
            0.036_632_8,
            0.111_280_7,
            0.216_745_2,
            0.270_682_2,
            0.216_745_2,
            0.111_280_7,
            0.036_632_8,
        ]
        .into_iter()
        .enumerate()
        {
            assert!((three[4 + entry] - expected).abs() < 1.0e-6, "k 3, {entry}");
        }
        for (k, expected_edge) in [
            (2, 0.054_488_57),
            (4, 0.027_630_49),
            (5, 0.022_190_5),
            (6, 0.018_543_98),
            (7, 0.015_928_36),
        ] {
            let taps = bloom_taps(k);
            close(taps[7 + k], expected_edge, "outer tap");
        }
        for k in 1..=7 {
            let taps = bloom_taps(k);
            let sum: f32 = taps.iter().sum();
            assert!((sum - 1.0).abs() < 2.0e-6, "radius {k} sums to {sum}");
            for u in 0..=7 {
                assert_eq!(taps[7 + u], taps[7 - u], "radius {k} is symmetric");
                assert_eq!(taps[7 + u] > 0.0, u <= k, "radius {k}, tap {u}");
            }
        }
    }

    #[test]
    fn the_bloom_radius_is_the_clamped_truncation() {
        for (radius, k) in [
            (-3.0, 1),
            (0.0, 1),
            (1.9, 1),
            (2.0, 2),
            (3.5, 3),
            (6.99, 6),
            (7.0, 7),
            (45.0, 7),
        ] {
            assert_eq!(bloom_radius(radius), k, "radius {radius}");
        }
        assert_eq!(bloom_radius(f32::NAN), 1);
    }

    /// `T` is the trimmed mean of the last 8 frame times (NEEDS 27).
    #[test]
    fn the_frame_time_is_a_trimmed_mean_of_eight() {
        let mut timer = FrameTimer::default();
        // Seven samples are still the 0.016 prefill: (7 x 0.016 + 0.020 - 0.020 - 0.016) / 6.
        close(timer.push(0.020), 0.016, "first frame");
        for _ in 0..8 {
            timer.push(1.0 / 60.0);
        }
        close(timer.push(1.0 / 60.0), 1.0 / 60.0, "steady 60 fps");
        // One spike above 0.05 s is only partly removed: (7 c + 0.1 - 0.05 - c) / 6.
        let c = 1.0 / 60.0;
        close(timer.push(0.1), (6.0 * c + 0.05) / 6.0, "spike");
        // A spike below the cap is removed in full.
        let mut timer = FrameTimer::default();
        for _ in 0..8 {
            timer.push(c);
        }
        close(timer.push(0.04), c, "a spike under 0.05 s leaves the mean");
        // A raw time above 1/6 s counts as 1/6 s.
        let mut a = FrameTimer::default();
        let mut b = FrameTimer::default();
        assert_eq!(a.push(5.0), b.push(MAX_FRAME_SECONDS));
        // A value that is not finite or is negative counts as zero and gives no negative T.
        let mut timer = FrameTimer::default();
        assert!(timer.push(f32::NAN) >= 0.0);
        assert!(timer.push(-1.0) >= 0.0);
    }
}
