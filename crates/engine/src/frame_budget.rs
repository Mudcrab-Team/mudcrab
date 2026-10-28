//! The frame-time distribution of a measured window set against a budget, and who owned the frames
//! that went over it.
//!
//! The goal this serves is a stable frame rate while travelling (the portal demo at 160 fps, a
//! budget of 6.25 ms per frame), which a mean or a p95 cannot show: one 40 ms hitch every few
//! seconds hides in both. The report therefore carries the tail (p99, p99.9, the worst frame), how
//! many frames went over the budget and how often, how many went over twice the budget, and, for
//! the frames over the budget, which side of the engine owned them.
//!
//! # Frame times
//!
//! The frame times this section scores are real-clock deltas (`Time<Real>`). Bevy's virtual clock
//! clamps a frame at 250 ms, and a clamp would cut off exactly the frames this section exists to
//! count. The older `frame_ms_*` fields keep the clock they had: the virtual one on a run without a
//! route, unchanged, and the real one on a route run, where the route's own timing needs it.
//!
//! # The owner of a frame
//!
//! A frame's duration is the time from the start of one main-thread frame to the start of the next.
//! With pipelined rendering (Bevy's default) the main thread runs its schedule for frame `n + 1`
//! while the render thread renders frame `n`, and the main thread then waits for the render thread
//! before it extracts, so the duration is roughly the larger of the two sides plus the extract. The
//! owner is the side with the largest measured time in that same cycle (see
//! `render_timing::CycleSides`, which keys every sample to the cycle it ran in):
//!
//! * `main_world`: the main thread: the main schedule plus the extract step, less the streaming
//!   commit time, which is counted as `upload`.
//! * `upload`: the streaming commit time of the frame (`streaming/frame_commit`: cells and LOD
//!   chunks committed, which hands meshes and textures to the renderer). It is measured on the main
//!   thread and only on frames that committed something. The renderer's own asset preparation runs
//!   on the render thread and is part of `render_thread`.
//! * `render_thread`: the render thread's CPU time (the render schedule), less the time it spent
//!   waiting for a swapchain image.
//! * `gpu`: the time the render thread waited for a swapchain image (`render/swapchain_acquire`),
//!   which is where a GPU-bound or presentation-bound frame shows up on the CPU. This is a
//!   lower bound for the GPU: a render thread that blocks in submit or present instead is counted as
//!   `render_thread`, because those waits are not split out of `render/graph_and_present`. GPU pass
//!   timestamps (`gpu_passes`) are deliberately not used to assign a frame: Bevy publishes them from a
//!   readback that arrives a frame or more late, with no frame number to match them by, so a
//!   per-frame match would be a guess.
//! * `unknown`: no owner could be named. Either the frame has no per-side data (the main-thread or
//!   render-thread sample for its cycle is missing, as for the first measured frame or a run with no
//!   renderer), or no side reaches half the frame (main-thread work and streaming upload can each be
//!   under half, with the rest operating-system or presentation time), in which case naming an owner
//!   would be a guess presented as a measurement.
//!
//! Ties go to the first of `main_world`, `render_thread`, `upload`, `gpu`.

use serde::Serialize;

/// The default frame budget: 160 frames per second.
pub const DEFAULT_FRAME_BUDGET_MS: f64 = 6.25;

/// The share of a frame its largest side must cover for the side to be named its owner.
const OWNER_MIN_SHARE: f64 = 0.5;

/// The rule above, as the report carries it so a reader of the JSON does not need this file.
pub const OWNER_RULE: &str = "Per frame (one main-thread cycle), the owner is the side with the largest measured time: \
main_world (main schedule + extract, less streaming commit), upload (streaming commit time), \
render_thread (render-schedule CPU time, less the swapchain-acquire wait) or gpu (the swapchain-acquire wait, \
a lower bound: submit and present waits are counted in render_thread; GPU pass timestamps are not matched \
to frames because their readback lags by an unknown number of frames). \
unknown: the cycle has no main-thread or render-thread sample, or no side reaches half the frame (main-thread work and streaming upload can each be under half). over_budget_main_thread_wait is a diagnostic, not an owner: for the frames over the budget, how long the main thread waited for the render thread to hand the render world back (the median and the largest wait of the frames that have one), which tells a busy render thread (large wait) from a blocked-elsewhere main thread (small wait).";

/// Who owned one frame; see the module documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameOwner {
    MainWorld,
    RenderThread,
    Gpu,
    Upload,
    Unknown,
}

/// The measured time of each side of one frame's cycle, in milliseconds. `None` is "not measured".
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FrameSides {
    /// Main schedule plus extract, less the commit time. `None` without a main-thread sample.
    pub main_world_ms: Option<f64>,
    /// Streaming commit time; `None` for a run with no streaming.
    pub upload_ms: Option<f64>,
    /// Render-schedule CPU time, less the swapchain wait. `None` without a render-thread sample.
    pub render_thread_ms: Option<f64>,
    /// The swapchain-image wait; `None` when the render thread did not record it.
    pub gpu_wait_ms: Option<f64>,
}

impl FrameSides {
    /// Builds the sides from the raw per-cycle samples: the main schedule, the extract, the render
    /// schedule, the swapchain wait, and the streaming commit time of the cycle.
    ///
    /// The commit time sits inside the main schedule and the swapchain wait inside the render
    /// schedule, so each is taken out of the side that contains it (never below zero), and every side
    /// is then a time spent on that side alone.
    pub fn from_samples(
        main_ms: Option<f64>,
        extract_ms: Option<f64>,
        render_ms: Option<f64>,
        acquire_ms: Option<f64>,
        upload_ms: Option<f64>,
    ) -> Self {
        let finite = |value: Option<f64>| value.filter(|value| value.is_finite() && *value >= 0.0);
        let (main_ms, extract_ms, render_ms, acquire_ms, upload_ms) = (
            finite(main_ms),
            finite(extract_ms),
            finite(render_ms),
            finite(acquire_ms),
            finite(upload_ms),
        );
        Self {
            main_world_ms: main_ms
                .map(|main| (main - upload_ms.unwrap_or(0.0)).max(0.0) + extract_ms.unwrap_or(0.0)),
            upload_ms,
            render_thread_ms: render_ms
                .map(|render| (render - acquire_ms.unwrap_or(0.0).min(render)).max(0.0)),
            gpu_wait_ms: acquire_ms,
        }
    }
}

/// The owner of a frame of `frame_ms` whose cycle measured `sides`; see the module documentation.
pub fn attribute(frame_ms: f64, sides: &FrameSides) -> FrameOwner {
    if !frame_ms.is_finite() || frame_ms <= 0.0 {
        return FrameOwner::Unknown;
    }
    // Both CPU sides must have a sample. With one of them missing the comparison would be between
    // a measured side and a hole.
    let (Some(main), Some(render)) = (sides.main_world_ms, sides.render_thread_ms) else {
        return FrameOwner::Unknown;
    };
    let candidates = [
        (FrameOwner::MainWorld, main),
        (FrameOwner::RenderThread, render),
        (FrameOwner::Upload, sides.upload_ms.unwrap_or(0.0)),
        (FrameOwner::Gpu, sides.gpu_wait_ms.unwrap_or(0.0)),
    ];
    let mut largest = candidates[0];
    for candidate in &candidates[1..] {
        if candidate.1 > largest.1 {
            largest = *candidate;
        }
    }
    if largest.1 < frame_ms * OWNER_MIN_SHARE {
        FrameOwner::Unknown
    } else {
        largest.0
    }
}

/// One scored frame: its duration and its owner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoredFrame {
    pub ms: f64,
    pub owner: FrameOwner,
    /// The main thread's wait for the render thread in the frame's cycle, when it was measured
    /// (pipelined rendering only). A diagnostic, not an input to the owner.
    pub wait_ms: Option<f64>,
}

/// The main thread's wait for the render thread over the frames that went over the budget.
#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct WaitSummary {
    /// Over-budget frames that have a wait measurement.
    pub frames: u64,
    /// Nearest rank, 50%.
    pub median_ms: f64,
    pub max_ms: f64,
}

/// How many frames over the budget each owner had.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
pub struct OwnerCounts {
    pub main_world: u64,
    pub render_thread: u64,
    pub gpu: u64,
    pub upload: u64,
    pub unknown: u64,
}

impl OwnerCounts {
    fn add(&mut self, owner: FrameOwner) {
        let count = match owner {
            FrameOwner::MainWorld => &mut self.main_world,
            FrameOwner::RenderThread => &mut self.render_thread,
            FrameOwner::Gpu => &mut self.gpu,
            FrameOwner::Upload => &mut self.upload,
            FrameOwner::Unknown => &mut self.unknown,
        };
        *count += 1;
    }

    /// Every counted frame.
    pub fn total(&self) -> u64 {
        self.main_world + self.render_thread + self.gpu + self.upload + self.unknown
    }
}

/// The frame-time distribution of a window against the budget (the report's `frame_budget`).
/// Percentiles are nearest rank over the scored frames; `null` means there was no frame to rank.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FrameBudgetReport {
    /// The budget the frames are counted against (`--frame-budget-ms`).
    pub budget_ms: f64,
    /// Which frames were scored.
    pub scope: String,
    /// The clock the frame times come from.
    pub clock: &'static str,
    /// Frames scored.
    pub frames: u64,
    /// The scored frames' total duration, in seconds.
    pub window_seconds: f64,
    /// Nearest rank: the smallest frame time with at least half of the frames at or below it.
    pub p50_ms: Option<f64>,
    /// Nearest rank, 99%. Not the same formula as the report's `frame_ms_p99` (which takes the higher
    /// of the two ranks around `(n - 1) * 0.99`); the two can differ by one rank.
    pub p99_ms: Option<f64>,
    /// Nearest rank, 99.9%.
    pub p99_9_ms: Option<f64>,
    pub worst_ms: Option<f64>,
    /// Frames longer than the budget.
    pub frames_over_budget: u64,
    /// The share of frames at or under the budget, 0 to 1.
    pub share_within_budget: Option<f64>,
    /// Frames over the budget per minute of the scored window.
    pub over_budget_per_minute: Option<f64>,
    /// Frames longer than twice the budget.
    pub frames_over_double_budget: u64,
    /// Whether the window met the proposed bar: at least 99.9% of frames within the budget and no
    /// frame over twice the budget. `null` without frames.
    pub meets_pass_bar: Option<bool>,
    /// The frames over the budget, by owner. Sums to `frames_over_budget`.
    pub over_budget_owners: OwnerCounts,
    /// The main thread's wait for the render thread over the frames over the budget, a diagnostic
    /// and not an owner: a large wait means the main thread sat blocked on a busy render thread, a
    /// small one that the render thread was not what held the frame. `null` when no over-budget
    /// frame has a wait measurement (no frame over the budget, or no pipelined rendering).
    pub over_budget_main_thread_wait: Option<WaitSummary>,
    /// How owners are assigned and what that cannot tell (see [`OWNER_RULE`]).
    pub owner_rule: &'static str,
}

/// The entry of `sorted` at nearest rank `numerator / denominator` (rank = `ceil(n * p)`, counted
/// from one), in integers so that 99.9% of 1000 frames is rank 999 and not whatever a float says.
fn nearest_rank(sorted: &[f64], numerator: usize, denominator: usize) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (sorted.len() * numerator).div_ceil(denominator);
    Some(sorted[rank.clamp(1, sorted.len()) - 1])
}

fn wait_summary(waits: &mut [f64]) -> Option<WaitSummary> {
    waits.sort_by(f64::total_cmp);
    Some(WaitSummary {
        frames: waits.len() as u64,
        median_ms: nearest_rank(waits, 50, 100)?,
        max_ms: *waits.last()?,
    })
}

impl FrameBudgetReport {
    pub fn from_frames(budget_ms: f64, scope: &str, frames: &[ScoredFrame]) -> Self {
        let mut sorted: Vec<f64> = frames.iter().map(|frame| frame.ms).collect();
        sorted.sort_by(f64::total_cmp);
        let count = frames.len();
        let window_seconds = sorted.iter().sum::<f64>() / 1000.0;
        let mut owners = OwnerCounts::default();
        let mut over = 0u64;
        let mut over_double = 0u64;
        let mut waits = Vec::new();
        for frame in frames {
            if frame.ms > budget_ms {
                over += 1;
                owners.add(frame.owner);
                waits.extend(frame.wait_ms.filter(|wait| wait.is_finite()));
            }
            if frame.ms > budget_ms * 2.0 {
                over_double += 1;
            }
        }
        let has_frames = count > 0;
        Self {
            budget_ms,
            scope: scope.to_owned(),
            clock: "real clock, unclamped",
            frames: count as u64,
            window_seconds,
            p50_ms: nearest_rank(&sorted, 50, 100),
            p99_ms: nearest_rank(&sorted, 99, 100),
            p99_9_ms: nearest_rank(&sorted, 999, 1000),
            worst_ms: sorted.last().copied(),
            frames_over_budget: over,
            share_within_budget: has_frames.then(|| (count as u64 - over) as f64 / count as f64),
            over_budget_per_minute: (has_frames && window_seconds > 0.0)
                .then(|| over as f64 / (window_seconds / 60.0)),
            frames_over_double_budget: over_double,
            meets_pass_bar: has_frames.then(|| over * 1000 <= count as u64 && over_double == 0),
            over_budget_owners: owners,
            over_budget_main_thread_wait: wait_summary(&mut waits),
            owner_rule: OWNER_RULE,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(ms: f64, owner: FrameOwner) -> ScoredFrame {
        ScoredFrame {
            ms,
            owner,
            wait_ms: None,
        }
    }

    fn frame_waiting(ms: f64, wait_ms: f64) -> ScoredFrame {
        ScoredFrame {
            wait_ms: Some(wait_ms),
            ..frame(ms, FrameOwner::RenderThread)
        }
    }

    /// The wait is summarised over the over-budget frames only, with the median by nearest rank.
    #[test]
    fn the_main_thread_wait_covers_the_over_budget_frames_only() {
        let report = FrameBudgetReport::from_frames(
            6.25,
            "test",
            &[
                // In budget: its wait is not counted.
                frame_waiting(5.0, 99.0),
                frame_waiting(8.0, 1.0),
                frame_waiting(9.0, 7.0),
                frame_waiting(10.0, 3.0),
                // Over budget with no wait measured: not in the summary.
                frame(11.0, FrameOwner::MainWorld),
            ],
        );
        assert_eq!(report.frames_over_budget, 4);
        let wait = report.over_budget_main_thread_wait.unwrap();
        assert_eq!(wait.frames, 3);
        assert_eq!(wait.median_ms, 3.0);
        assert_eq!(wait.max_ms, 7.0);
        // Nothing over budget, or no wait data: no summary rather than zeros.
        let quiet = FrameBudgetReport::from_frames(6.25, "test", &[frame_waiting(5.0, 1.0)]);
        assert_eq!(quiet.over_budget_main_thread_wait, None);
        let none =
            FrameBudgetReport::from_frames(6.25, "test", &[frame(9.0, FrameOwner::RenderThread)]);
        assert_eq!(none.over_budget_main_thread_wait, None);
    }

    fn sides(main: f64, render: f64, upload: f64, gpu: f64) -> FrameSides {
        FrameSides {
            main_world_ms: Some(main),
            upload_ms: Some(upload),
            render_thread_ms: Some(render),
            gpu_wait_ms: Some(gpu),
        }
    }

    #[test]
    fn the_largest_side_owns_the_frame() {
        assert_eq!(
            attribute(10.0, &sides(9.0, 2.0, 0.5, 0.2)),
            FrameOwner::MainWorld
        );
        assert_eq!(
            attribute(10.0, &sides(2.0, 9.0, 0.5, 0.2)),
            FrameOwner::RenderThread
        );
        assert_eq!(
            attribute(10.0, &sides(2.0, 3.0, 8.0, 0.2)),
            FrameOwner::Upload
        );
        assert_eq!(attribute(10.0, &sides(2.0, 3.0, 0.5, 8.0)), FrameOwner::Gpu);
    }

    #[test]
    fn ties_go_to_the_first_side_in_the_documented_order() {
        assert_eq!(
            attribute(10.0, &sides(6.0, 6.0, 6.0, 6.0)),
            FrameOwner::MainWorld
        );
        assert_eq!(
            attribute(10.0, &sides(1.0, 6.0, 6.0, 6.0)),
            FrameOwner::RenderThread
        );
    }

    /// A frame with no per-side data has no owner, and so does one no side of which reaches half of
    /// it: main-thread work and streaming upload can each be under half.
    #[test]
    fn a_frame_without_data_or_explained_by_less_than_half_is_unknown() {
        assert_eq!(attribute(10.0, &FrameSides::default()), FrameOwner::Unknown);
        let no_render = FrameSides {
            main_world_ms: Some(9.0),
            ..FrameSides::default()
        };
        assert_eq!(attribute(10.0, &no_render), FrameOwner::Unknown);
        let no_main = FrameSides {
            render_thread_ms: Some(9.0),
            ..FrameSides::default()
        };
        assert_eq!(attribute(10.0, &no_main), FrameOwner::Unknown);
        // The largest side, 4.9 ms, is just under half of the 10 ms frame.
        assert_eq!(
            attribute(10.0, &sides(4.9, 3.0, 0.0, 0.0)),
            FrameOwner::Unknown
        );
        // Exactly half is enough.
        assert_eq!(
            attribute(10.0, &sides(5.0, 3.0, 0.0, 0.0)),
            FrameOwner::MainWorld
        );
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                attribute(bad, &sides(9.0, 9.0, 9.0, 9.0)),
                FrameOwner::Unknown
            );
        }
    }

    /// A run with no streaming has no upload time and a render thread that never recorded the wait:
    /// those sides count as zero, and the CPU sides still decide.
    #[test]
    fn missing_upload_and_wait_do_not_stop_the_cpu_sides_deciding() {
        let sides = FrameSides {
            main_world_ms: Some(2.0),
            render_thread_ms: Some(8.0),
            ..FrameSides::default()
        };
        assert_eq!(attribute(9.0, &sides), FrameOwner::RenderThread);
    }

    #[test]
    fn the_commit_and_the_wait_are_taken_out_of_the_side_that_contains_them() {
        let built = FrameSides::from_samples(Some(5.0), Some(0.5), Some(4.0), Some(1.5), Some(3.0));
        // 5.0 - 3.0 + 0.5 on the main thread, 4.0 - 1.5 on the render thread.
        assert_eq!(built.main_world_ms, Some(2.5));
        assert_eq!(built.upload_ms, Some(3.0));
        assert_eq!(built.render_thread_ms, Some(2.5));
        assert_eq!(built.gpu_wait_ms, Some(1.5));
        // A commit longer than the main schedule (the two clocks started at different instants)
        // cannot make the side negative.
        let clipped = FrameSides::from_samples(Some(1.0), None, Some(1.0), Some(5.0), Some(4.0));
        assert_eq!(clipped.main_world_ms, Some(0.0));
        assert_eq!(clipped.render_thread_ms, Some(0.0));
        // No main sample, no main side; a non-finite value is a missing one.
        let missing = FrameSides::from_samples(None, Some(1.0), Some(f64::NAN), None, None);
        assert_eq!(missing.main_world_ms, None);
        assert_eq!(missing.render_thread_ms, None);
    }

    #[test]
    fn nearest_rank_percentiles_follow_the_integer_rank() {
        let frames: Vec<ScoredFrame> = (1..=1000)
            .map(|n| frame(f64::from(n), FrameOwner::Unknown))
            .collect();
        let report = FrameBudgetReport::from_frames(6.25, "test", &frames);
        assert_eq!(report.frames, 1000);
        // rank ceil(1000 * 0.999) = 999, so the 999th frame, not the worst.
        assert_eq!(report.p99_9_ms, Some(999.0));
        assert_eq!(report.p99_ms, Some(990.0));
        assert_eq!(report.p50_ms, Some(500.0));
        assert_eq!(report.worst_ms, Some(1000.0));
        // One frame: every percentile is that frame.
        let single =
            FrameBudgetReport::from_frames(6.25, "test", &[frame(7.0, FrameOwner::MainWorld)]);
        assert_eq!(
            (
                single.p50_ms,
                single.p99_ms,
                single.p99_9_ms,
                single.worst_ms
            ),
            (Some(7.0), Some(7.0), Some(7.0), Some(7.0))
        );
        // Ranks are of the sorted frames, whatever order they arrived in.
        let shuffled = FrameBudgetReport::from_frames(
            6.25,
            "test",
            &[
                frame(9.0, FrameOwner::Unknown),
                frame(1.0, FrameOwner::Unknown),
                frame(5.0, FrameOwner::Unknown),
            ],
        );
        assert_eq!(shuffled.p50_ms, Some(5.0));
        assert_eq!(shuffled.p99_9_ms, Some(9.0));
    }

    #[test]
    fn budget_counts_the_rate_and_the_owners() {
        // 10 frames: 6.25 is not over the budget, 12.5 is not over twice it.
        let frames = [
            frame(5.0, FrameOwner::MainWorld),
            frame(6.25, FrameOwner::MainWorld),
            frame(6.26, FrameOwner::RenderThread),
            frame(8.0, FrameOwner::RenderThread),
            frame(12.5, FrameOwner::Upload),
            frame(12.51, FrameOwner::Gpu),
            frame(40.0, FrameOwner::Unknown),
            frame(4.0, FrameOwner::Gpu),
            frame(4.0, FrameOwner::Gpu),
            frame(4.0, FrameOwner::Gpu),
        ];
        let report = FrameBudgetReport::from_frames(6.25, "window", &frames);
        assert_eq!(report.frames, 10);
        assert_eq!(report.frames_over_budget, 5);
        assert_eq!(report.frames_over_double_budget, 2);
        assert_eq!(report.share_within_budget, Some(0.5));
        // Owners are counted for the frames over the budget only; the in-budget Gpu frames are not.
        assert_eq!(
            report.over_budget_owners,
            OwnerCounts {
                main_world: 0,
                render_thread: 2,
                gpu: 1,
                upload: 1,
                unknown: 1,
            }
        );
        assert_eq!(report.over_budget_owners.total(), report.frames_over_budget);
        assert_eq!(report.meets_pass_bar, Some(false));
        // 5 over in 0.10236 s: per minute is over / (seconds / 60).
        let seconds = (5.0 + 6.25 + 6.26 + 8.0 + 12.5 + 12.51 + 40.0 + 12.0) / 1000.0;
        assert!((report.window_seconds - seconds).abs() < 1e-12);
        let rate = report.over_budget_per_minute.unwrap();
        assert!((rate - 5.0 / (seconds / 60.0)).abs() < 1e-9);
    }

    #[test]
    fn the_pass_bar_allows_one_in_a_thousand_over_and_none_over_double() {
        let mut frames = vec![frame(5.0, FrameOwner::Unknown); 999];
        frames.push(frame(7.0, FrameOwner::MainWorld));
        let report = FrameBudgetReport::from_frames(6.25, "window", &frames);
        assert_eq!(report.frames_over_budget, 1);
        assert_eq!(report.meets_pass_bar, Some(true), "1 in 1000 is 99.9%");
        frames.push(frame(7.0, FrameOwner::MainWorld));
        let report = FrameBudgetReport::from_frames(6.25, "window", &frames);
        assert_eq!(
            report.meets_pass_bar,
            Some(false),
            "2 in 1001 is under 99.9%"
        );
        let mut frames = vec![frame(5.0, FrameOwner::Unknown); 5000];
        frames.push(frame(13.0, FrameOwner::MainWorld));
        let report = FrameBudgetReport::from_frames(6.25, "window", &frames);
        assert_eq!(report.frames_over_double_budget, 1);
        assert_eq!(
            report.meets_pass_bar,
            Some(false),
            "one frame over twice the budget fails the bar however rare"
        );
    }

    #[test]
    fn no_frames_report_nothing_rather_than_zeros() {
        let report = FrameBudgetReport::from_frames(6.25, "empty", &[]);
        assert_eq!(report.frames, 0);
        assert_eq!(report.p50_ms, None);
        assert_eq!(report.p99_ms, None);
        assert_eq!(report.p99_9_ms, None);
        assert_eq!(report.worst_ms, None);
        assert_eq!(report.share_within_budget, None);
        assert_eq!(report.over_budget_per_minute, None);
        assert_eq!(report.meets_pass_bar, None);
        assert_eq!(report.frames_over_budget, 0);
        let json = serde_json::to_value(&report).unwrap();
        assert!(json["p99_9_ms"].is_null());
        assert_eq!(json["over_budget_owners"]["unknown"], 0);
    }
}
