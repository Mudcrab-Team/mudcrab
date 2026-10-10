//! Finite streaming budgets and hysteresis, independent of ECS and loader state.
//!
//! Every zero here means no work. The caller must translate legacy unlimited
//! settings into finite adaptive ceilings before invoking the controller.

use serde::Serialize;
use std::collections::VecDeque;

const MIB: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct StageBudgets {
    pub max_scene_jobs: usize,
    pub max_model_activations: usize,
    pub max_cell_commits: usize,
    pub max_upload_bytes_per_frame: u64,
    pub max_commit_micros: u64,
    pub max_activation_micros: u64,
    pub max_collision_micros: u64,
    pub max_admission_micros: u64,
}

impl Default for StageBudgets {
    fn default() -> Self {
        Self {
            max_scene_jobs: 64,
            max_model_activations: 32,
            max_cell_commits: 1,
            max_upload_bytes_per_frame: 16 * MIB,
            max_commit_micros: 16_670,
            max_activation_micros: 4_000,
            max_collision_micros: 4_000,
            max_admission_micros: 2_000,
        }
    }
}

impl StageBudgets {
    pub fn bounded_by(self, ceiling: Self) -> Self {
        Self {
            max_scene_jobs: self.max_scene_jobs.min(ceiling.max_scene_jobs),
            max_model_activations: self
                .max_model_activations
                .min(ceiling.max_model_activations),
            max_cell_commits: self.max_cell_commits.min(ceiling.max_cell_commits),
            max_upload_bytes_per_frame: self
                .max_upload_bytes_per_frame
                .min(ceiling.max_upload_bytes_per_frame),
            max_commit_micros: self.max_commit_micros.min(ceiling.max_commit_micros),
            max_activation_micros: self
                .max_activation_micros
                .min(ceiling.max_activation_micros),
            max_collision_micros: self.max_collision_micros.min(ceiling.max_collision_micros),
            max_admission_micros: self.max_admission_micros.min(ceiling.max_admission_micros),
        }
    }

    fn blend(self, toward: Self, fraction: f64) -> Self {
        let count =
            |a: usize, b: usize| (a as f64 + (b as f64 - a as f64) * fraction).round() as usize;
        let bytes = |a: u64, b: u64| (a as f64 + (b as f64 - a as f64) * fraction).round() as u64;
        Self {
            max_scene_jobs: count(self.max_scene_jobs, toward.max_scene_jobs),
            max_model_activations: count(self.max_model_activations, toward.max_model_activations),
            max_cell_commits: count(self.max_cell_commits, toward.max_cell_commits),
            max_upload_bytes_per_frame: bytes(
                self.max_upload_bytes_per_frame,
                toward.max_upload_bytes_per_frame,
            ),
            max_commit_micros: bytes(self.max_commit_micros, toward.max_commit_micros),
            max_activation_micros: bytes(self.max_activation_micros, toward.max_activation_micros),
            max_collision_micros: bytes(self.max_collision_micros, toward.max_collision_micros),
            max_admission_micros: bytes(self.max_admission_micros, toward.max_admission_micros),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct DownstreamBacklog {
    /// CPU-ready placements waiting for activation, excluding unadmitted loads.
    pub ready_placements: usize,
    pub spawned_instances: usize,
    pub collider_jobs: usize,
    pub gpu_assets: usize,
    pub gpu_bytes: u64,
    /// Responses already returned by the database and waiting to commit.
    pub cell_responses: usize,
}

impl DownstreamBacklog {
    fn any_at_or_above(self, high: Self) -> bool {
        // A zero high watermark disables that observation; it must not make an
        // empty or unavailable queue pressure the controller forever.
        (high.ready_placements != 0 && self.ready_placements >= high.ready_placements)
            || (high.spawned_instances != 0 && self.spawned_instances >= high.spawned_instances)
            || (high.collider_jobs != 0 && self.collider_jobs >= high.collider_jobs)
            || (high.gpu_assets != 0 && self.gpu_assets >= high.gpu_assets)
            || (high.gpu_bytes != 0 && self.gpu_bytes >= high.gpu_bytes)
            || (high.cell_responses != 0 && self.cell_responses >= high.cell_responses)
    }

    fn all_at_or_below(self, low: Self, high: Self) -> bool {
        (high.ready_placements == 0 || self.ready_placements <= low.ready_placements)
            && (high.spawned_instances == 0 || self.spawned_instances <= low.spawned_instances)
            && (high.collider_jobs == 0 || self.collider_jobs <= low.collider_jobs)
            && (high.gpu_assets == 0 || self.gpu_assets <= low.gpu_assets)
            && (high.gpu_bytes == 0 || self.gpu_bytes <= low.gpu_bytes)
            && (high.cell_responses == 0 || self.cell_responses <= low.cell_responses)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ControllerSettings {
    /// False applies only queue/memory backpressure to `fixed` budgets.
    pub adaptive: bool,
    pub fixed: StageBudgets,
    pub startup: StageBudgets,
    pub cruise: StageBudgets,
    pub quiet: StageBudgets,
    pub minimum: StageBudgets,
    pub high_watermarks: DownstreamBacklog,
    pub low_watermarks: DownstreamBacklog,
    pub target_frame_ms: f64,
    pub timing_smoothing_seconds: f64,
    pub pressure_enter_ratio: f64,
    pub spike_ratio: f64,
    pub recovery_hold_seconds: f64,
    /// Interval for one ordinary scene job when frame pressure alone persists.
    /// This never bypasses memory or downstream low watermarks.
    pub recovery_probe_seconds: f64,
    pub motion_hold_seconds: f64,
    pub quiet_delay_seconds: f64,
    pub quiet_ramp_seconds: f64,
    pub startup_ready_hold_seconds: f64,
    /// Share of measured frame headroom offered to synchronous streaming work.
    pub streaming_headroom_fraction: f64,
    pub mandatory_collision_jobs: usize,
    pub mandatory_collision_activations: usize,
}

impl Default for ControllerSettings {
    fn default() -> Self {
        let fixed = StageBudgets::default();
        Self {
            adaptive: false,
            fixed,
            startup: StageBudgets {
                max_scene_jobs: 128,
                max_model_activations: 96,
                max_cell_commits: 2,
                max_upload_bytes_per_frame: 32 * MIB,
                max_commit_micros: 4_000,
                max_activation_micros: 2_000,
                max_collision_micros: 2_000,
                max_admission_micros: 1_000,
            },
            cruise: StageBudgets {
                max_scene_jobs: 16,
                max_model_activations: 8,
                max_cell_commits: 1,
                max_upload_bytes_per_frame: 4 * MIB,
                max_commit_micros: 1_000,
                max_activation_micros: 500,
                max_collision_micros: 500,
                max_admission_micros: 500,
            },
            quiet: fixed,
            minimum: StageBudgets {
                max_scene_jobs: 4,
                max_model_activations: 2,
                max_cell_commits: 1,
                max_upload_bytes_per_frame: MIB,
                max_commit_micros: 250,
                max_activation_micros: 200,
                max_collision_micros: 250,
                max_admission_micros: 200,
            },
            high_watermarks: DownstreamBacklog {
                ready_placements: 256,
                spawned_instances: 128,
                collider_jobs: 128,
                gpu_assets: 256,
                gpu_bytes: 128 * MIB,
                cell_responses: 16,
            },
            low_watermarks: DownstreamBacklog {
                ready_placements: 64,
                spawned_instances: 32,
                collider_jobs: 32,
                gpu_assets: 64,
                gpu_bytes: 32 * MIB,
                cell_responses: 4,
            },
            target_frame_ms: 16.67,
            timing_smoothing_seconds: 0.5,
            pressure_enter_ratio: 1.15,
            spike_ratio: 1.5,
            recovery_hold_seconds: 0.5,
            recovery_probe_seconds: 2.0,
            motion_hold_seconds: 0.35,
            quiet_delay_seconds: 0.75,
            quiet_ramp_seconds: 2.0,
            startup_ready_hold_seconds: 0.25,
            streaming_headroom_fraction: 0.5,
            mandatory_collision_jobs: 1,
            mandatory_collision_activations: 1,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ControllerInput {
    pub delta_seconds: f64,
    pub frame_ms: Option<f64>,
    /// Supply a fresh sample only; unavailable or nonpositive timings are ignored.
    pub gpu_frame_ms: Option<f64>,
    pub streaming_work_ms: f64,
    pub camera_moving: bool,
    pub camera_turning: bool,
    /// Teleport or camera/world replacement, not a render-origin rebase.
    pub camera_discontinuity: bool,
    pub useful_nearby_ready: bool,
    pub demand_settled: bool,
    pub backlog: DownstreamBacklog,
    pub active_scene_jobs: usize,
    /// Finite upper bounds from configured ceilings and resource admission.
    pub hard_limits: StageBudgets,
    pub memory_blocked: bool,
    pub mandatory_collision_pending: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ControllerMode {
    Fixed,
    Startup,
    Cruise,
    Quiet,
    Recovery,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct ControllerReasons {
    pub queue_pressure: bool,
    pub memory_pressure: bool,
    pub frame_pressure: bool,
    pub frame_spike: bool,
    pub recovery_hold: bool,
    pub camera_motion: bool,
    pub camera_discontinuity: bool,
    pub startup_useful: bool,
    pub demand_settled: bool,
    pub scheduling_floor: bool,
    pub recovery_probe: bool,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub(crate) struct ControllerDecision {
    pub mode: ControllerMode,
    pub reasons: ControllerReasons,
    pub budgets: StageBudgets,
    pub allow_scene_intake: bool,
    pub allow_cell_requests: bool,
    /// Additional collision-only dispatches while ordinary intake is paused.
    /// The caller must still reserve each resource and enforce the hard job cap.
    pub mandatory_scene_jobs: usize,
    pub mandatory_model_activations: usize,
    pub frame_ema_ms: Option<f64>,
    pub gpu_frame_ema_ms: Option<f64>,
    pub non_streaming_ema_ms: Option<f64>,
    pub streaming_work_ema_ms: Option<f64>,
    pub configured_target_frame_ms: f64,
    pub effective_target_frame_ms: f64,
    /// Median recent non-streaming cost; short asynchronous frames are outliers.
    pub recent_base_frame_ms: Option<f64>,
    /// Recent non-streaming 95th percentile, including ordinary frame variance.
    pub recent_base_upper_ms: Option<f64>,
    pub frame_spike_threshold_ms: f64,
    pub quiet_seconds: f64,
    pub startup_complete: bool,
    pub frame_samples: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct ControllerState {
    frame_ema_ms: Option<f64>,
    gpu_frame_ema_ms: Option<f64>,
    gpu_sample_age_seconds: f64,
    non_streaming_ema_ms: Option<f64>,
    streaming_work_ema_ms: Option<f64>,
    baseline_samples: VecDeque<(f64, f64)>,
    elapsed_seconds: f64,
    recovery: bool,
    recovery_reduced: bool,
    recovering_low_seconds: f64,
    recovering_drain_seconds: f64,
    motion_remaining_seconds: f64,
    quiet_seconds: f64,
    useful_seconds: f64,
    startup_complete: bool,
    frame_samples: u64,
    ramp_fraction: f64,
    recovery_probe_elapsed: f64,
}

impl Default for ControllerState {
    fn default() -> Self {
        Self {
            frame_ema_ms: None,
            gpu_frame_ema_ms: None,
            gpu_sample_age_seconds: 0.0,
            non_streaming_ema_ms: None,
            streaming_work_ema_ms: None,
            baseline_samples: VecDeque::new(),
            elapsed_seconds: 0.0,
            recovery: false,
            recovery_reduced: false,
            recovering_low_seconds: 0.0,
            recovering_drain_seconds: 0.0,
            motion_remaining_seconds: 0.0,
            quiet_seconds: 0.0,
            useful_seconds: 0.0,
            startup_complete: false,
            frame_samples: 0,
            ramp_fraction: 1.0,
            recovery_probe_elapsed: 0.0,
        }
    }
}

fn positive_or(value: f64, fallback: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        fallback
    }
}

fn duration(value: f64) -> f64 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn sample(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite() && *value > 0.0)
}

fn update_ema(ema: &mut Option<f64>, value: Option<f64>, alpha: f64) {
    if let Some(value) = sample(value) {
        *ema = Some(ema.map_or(value, |previous| previous + (value - previous) * alpha));
    }
}

fn median(sorted: &[f64]) -> Option<f64> {
    let middle = sorted.len() / 2;
    if sorted.is_empty() {
        None
    } else if sorted.len().is_multiple_of(2) {
        Some(sorted[middle - 1] * 0.5 + sorted[middle] * 0.5)
    } else {
        Some(sorted[middle])
    }
}

impl ControllerState {
    pub fn tick(
        &mut self,
        settings: &ControllerSettings,
        input: ControllerInput,
    ) -> ControllerDecision {
        // A stalled frame must not count as a long quiet/recovery interval.
        let delta = duration(input.delta_seconds).min(0.25);
        let target = positive_or(settings.target_frame_ms, 16.67);
        self.elapsed_seconds += delta;
        let alpha = 1.0 - (-delta / positive_or(settings.timing_smoothing_seconds, 0.5)).exp();
        if input.camera_discontinuity {
            self.frame_ema_ms = None;
            self.gpu_frame_ema_ms = None;
            self.non_streaming_ema_ms = None;
            self.streaming_work_ema_ms = None;
            self.baseline_samples.clear();
            self.quiet_seconds = 0.0;
            self.motion_remaining_seconds = duration(settings.motion_hold_seconds);
            self.useful_seconds = 0.0;
            self.ramp_fraction = 0.0;
            // A midgame teleport does not re-enter initial-startup mode. Nearby
            // demand still uses cruise budgets until stationary headroom returns.
        }
        let frame_sample = sample(input.frame_ms);
        let streaming_work_ms = duration(input.streaming_work_ms);
        let non_streaming_sample = frame_sample.map(|frame| (frame - streaming_work_ms).max(0.001));
        update_ema(&mut self.frame_ema_ms, frame_sample, alpha);
        if sample(input.gpu_frame_ms).is_some() {
            self.gpu_sample_age_seconds = 0.0;
        } else {
            self.gpu_sample_age_seconds += delta;
            if self.gpu_sample_age_seconds > 2.0 {
                self.gpu_frame_ema_ms = None;
            }
        }
        update_ema(&mut self.gpu_frame_ema_ms, input.gpu_frame_ms, alpha);
        update_ema(&mut self.non_streaming_ema_ms, non_streaming_sample, alpha);
        update_ema(
            &mut self.streaming_work_ema_ms,
            Some(streaming_work_ms.max(0.001)),
            alpha,
        );
        let baseline_sample = non_streaming_sample
            .into_iter()
            .chain(sample(input.gpu_frame_ms))
            .reduce(f64::max);
        if let Some(value) = baseline_sample {
            self.baseline_samples
                .push_back((self.elapsed_seconds, value));
        }
        while self.baseline_samples.front().is_some_and(|(time, _)| {
            self.elapsed_seconds - time > 2.0 || self.baseline_samples.len() > 256
        }) {
            self.baseline_samples.pop_front();
        }
        let mut base_costs: Vec<_> = self
            .baseline_samples
            .iter()
            .map(|(_, value)| *value)
            .collect();
        base_costs.sort_unstable_by(f64::total_cmp);
        let recent_base_frame_ms = median(&base_costs);
        let recent_base_upper_ms = base_costs
            .len()
            .checked_sub(1)
            .map(|last| base_costs[(last as f64 * 0.95).floor() as usize]);
        // A GPU-bound machine may never meet the requested target. Compare new
        // load spikes with typical recent base cost instead of starving intake.
        // A few short asynchronous frames must not pin the reference below the
        // cost at which this scene normally renders.
        let effective_target = target.max(recent_base_frame_ms.unwrap_or(0.0) * 1.1);
        // Asynchronous presentation has a heavy upper tail. A spike must exceed
        // the usual slow frames, not just the median frame. The lower percentile
        // index also keeps one new outlier from setting its own short-history
        // threshold. Material streaming CPU work is checked independently below.
        let frame_spike_threshold = effective_target.max(recent_base_upper_ms.unwrap_or(0.0))
            * positive_or(settings.spike_ratio, 1.5);
        if frame_sample.is_some() {
            self.frame_samples = self.frame_samples.saturating_add(1);
        }

        let observed_ms = self
            .frame_ema_ms
            .into_iter()
            .chain(self.gpu_frame_ema_ms)
            .reduce(f64::max);
        let current_ms = frame_sample
            .into_iter()
            .chain(sample(input.gpu_frame_ms))
            .reduce(f64::max);
        if input.camera_moving || input.camera_turning {
            self.motion_remaining_seconds = duration(settings.motion_hold_seconds);
        } else {
            self.motion_remaining_seconds = (self.motion_remaining_seconds - delta).max(0.0);
        }
        let moving =
            input.camera_moving || input.camera_turning || self.motion_remaining_seconds > 0.0;
        let stationary_startup = !self.startup_complete && !moving;
        let enter_ratio = positive_or(settings.pressure_enter_ratio, 1.15).max(1.0);
        // Current work must be substantial before a moderate frame overshoot
        // pauses intake. Routine reconciliation and a stale work EMA are not a
        // new load spike. Stationary startup permits a larger CPU burst; walking
        // and all later loading keep their smaller material/immediate thresholds.
        let material_fraction = if stationary_startup { 0.5 } else { 0.25 };
        let immediate_fraction = if stationary_startup { 1.0 } else { 0.5 };
        let material_streaming = streaming_work_ms >= target * material_fraction;
        let frame_pressure = settings.adaptive
            && (streaming_work_ms >= target * immediate_fraction
                || (material_streaming
                    && observed_ms
                        .into_iter()
                        .chain(current_ms)
                        .any(|ms| ms >= effective_target * enter_ratio)));
        let frame_spike =
            settings.adaptive && current_ms.is_some_and(|ms| ms >= frame_spike_threshold);
        let queue_pressure = input.backlog.any_at_or_above(settings.high_watermarks);
        let reduced_pressure = input.memory_blocked || frame_pressure || frame_spike;
        let pressure = queue_pressure || reduced_pressure;
        // Use the same timing predicates for entry and recovery. A separate
        // below-target exit or lower CPU threshold can be permanently impossible
        // even after all entry-pressure signals clear. The stable hold provides
        // hysteresis without requiring an unreachable frame or polling cost.
        let timing_recovered = !frame_pressure && !frame_spike;

        // Intake hysteresis waits for the queues to drain. Reduced processing
        // budgets instead wait for timing/memory to recover, even while a large
        // ready queue still needs healthy throughput to reach its low watermark.
        if reduced_pressure {
            self.recovery_reduced = true;
            self.recovering_drain_seconds = 0.0;
        } else if self.recovery_reduced {
            if timing_recovered {
                self.recovering_drain_seconds += delta;
                if self.recovering_drain_seconds + f64::EPSILON
                    >= duration(settings.recovery_hold_seconds)
                {
                    self.recovery_reduced = false;
                    self.recovering_drain_seconds = 0.0;
                }
            } else {
                self.recovering_drain_seconds = 0.0;
            }
        }
        if pressure {
            self.recovery = true;
            self.recovering_low_seconds = 0.0;
            self.ramp_fraction = 0.0;
        } else if self.recovery {
            if timing_recovered
                && input
                    .backlog
                    .all_at_or_below(settings.low_watermarks, settings.high_watermarks)
            {
                self.recovering_low_seconds += delta;
                if self.recovering_low_seconds >= duration(settings.recovery_hold_seconds) {
                    self.recovery = false;
                    self.recovery_reduced = false;
                    self.recovering_low_seconds = 0.0;
                    self.recovering_drain_seconds = 0.0;
                }
            } else {
                self.recovering_low_seconds = 0.0;
            }
        }

        let queues_low = input
            .backlog
            .all_at_or_below(settings.low_watermarks, settings.high_watermarks);
        if self.recovery && !input.memory_blocked && queues_low {
            self.recovery_probe_elapsed += delta;
        } else {
            self.recovery_probe_elapsed = 0.0;
        }
        let recovery_probe = self.recovery
            && !input.memory_blocked
            && queues_low
            && input.active_scene_jobs < input.hard_limits.max_scene_jobs
            && self.recovery_probe_elapsed >= positive_or(settings.recovery_probe_seconds, 2.0);
        if recovery_probe {
            self.recovery_probe_elapsed = 0.0;
        }

        if moving || self.recovery {
            self.quiet_seconds = 0.0;
        } else {
            self.quiet_seconds += delta;
        }

        if !self.startup_complete {
            if (input.useful_nearby_ready || input.demand_settled)
                && !queue_pressure
                && !input.memory_blocked
                && !self.recovery
            {
                self.useful_seconds += delta;
                if self.useful_seconds >= duration(settings.startup_ready_hold_seconds) {
                    self.startup_complete = true;
                    self.ramp_fraction = 0.0;
                }
            } else {
                self.useful_seconds = 0.0;
            }
        }

        let quiet_delay = duration(settings.quiet_delay_seconds);
        let mode = if self.recovery {
            ControllerMode::Recovery
        } else if !settings.adaptive {
            ControllerMode::Fixed
        } else if moving {
            ControllerMode::Cruise
        } else if !self.startup_complete {
            ControllerMode::Startup
        } else if self.quiet_seconds >= quiet_delay {
            ControllerMode::Quiet
        } else {
            ControllerMode::Cruise
        };
        let desired = match mode {
            ControllerMode::Fixed => settings.fixed,
            ControllerMode::Recovery if self.recovery_reduced => settings.minimum,
            ControllerMode::Recovery if !settings.adaptive => settings.fixed,
            ControllerMode::Recovery if moving => settings.cruise,
            ControllerMode::Recovery if !self.startup_complete => settings.startup,
            ControllerMode::Recovery => settings.cruise,
            ControllerMode::Cruise => settings.cruise,
            ControllerMode::Startup => settings.startup,
            ControllerMode::Quiet => settings.cruise.blend(
                settings.quiet,
                ((self.quiet_seconds - quiet_delay)
                    / positive_or(settings.quiet_ramp_seconds, 2.0))
                .clamp(0.0, 1.0),
            ),
        };
        // Increases are gradual after pressure or a camera discontinuity. Motion
        // immediately lowers the ceiling; it never waits for this ramp.
        if !self.recovery {
            self.ramp_fraction = (self.ramp_fraction
                + delta / positive_or(settings.quiet_ramp_seconds, 2.0))
            .min(1.0);
        }
        let mut budgets = if settings.adaptive && mode != ControllerMode::Recovery {
            settings
                .minimum
                .bounded_by(desired)
                .blend(desired, self.ramp_fraction)
        } else {
            desired
        }
        .bounded_by(input.hard_limits);

        let scheduling_floor = if settings.adaptive {
            self.apply_timing_allowance(settings, input, &mut budgets, effective_target)
        } else {
            false
        };
        let allow_scene_intake = ((!self.recovery && budgets.max_scene_jobs != 0)
            || recovery_probe)
            && input.hard_limits.max_scene_jobs != 0;
        let allow_cell_requests = !self.recovery && budgets.max_cell_commits != 0;
        if !allow_scene_intake {
            // Never hand this zero to a legacy API where it means unlimited.
            budgets.max_scene_jobs = 0;
        } else if recovery_probe {
            budgets.max_scene_jobs = input
                .active_scene_jobs
                .saturating_add(1)
                .min(input.hard_limits.max_scene_jobs);
        }
        let mandatory_scene_jobs = if self.recovery
            && !allow_scene_intake
            && input.mandatory_collision_pending
            && !input.memory_blocked
        {
            settings.mandatory_collision_jobs.min(
                input
                    .hard_limits
                    .max_scene_jobs
                    .saturating_sub(input.active_scene_jobs),
            )
        } else {
            0
        };
        let mandatory_model_activations = if input.mandatory_collision_pending {
            settings
                .mandatory_collision_activations
                .min(budgets.max_model_activations)
        } else {
            0
        };
        ControllerDecision {
            mode,
            reasons: ControllerReasons {
                queue_pressure,
                memory_pressure: input.memory_blocked,
                frame_pressure,
                frame_spike,
                recovery_hold: self.recovery && !pressure,
                camera_motion: moving,
                camera_discontinuity: input.camera_discontinuity,
                startup_useful: input.useful_nearby_ready,
                demand_settled: input.demand_settled,
                scheduling_floor,
                recovery_probe,
            },
            budgets,
            allow_scene_intake,
            allow_cell_requests,
            mandatory_scene_jobs,
            mandatory_model_activations,
            frame_ema_ms: self.frame_ema_ms,
            gpu_frame_ema_ms: self.gpu_frame_ema_ms,
            non_streaming_ema_ms: self.non_streaming_ema_ms,
            streaming_work_ema_ms: self.streaming_work_ema_ms,
            configured_target_frame_ms: target,
            effective_target_frame_ms: effective_target,
            recent_base_frame_ms,
            recent_base_upper_ms,
            frame_spike_threshold_ms: frame_spike_threshold,
            quiet_seconds: self.quiet_seconds,
            startup_complete: self.startup_complete,
            frame_samples: self.frame_samples,
        }
    }

    fn apply_timing_allowance(
        &self,
        settings: &ControllerSettings,
        input: ControllerInput,
        budgets: &mut StageBudgets,
        effective_target: f64,
    ) -> bool {
        let non_streaming = self.non_streaming_ema_ms.unwrap_or(0.0);
        let fraction = if settings.streaming_headroom_fraction.is_finite() {
            settings.streaming_headroom_fraction.clamp(0.0, 1.0)
        } else {
            0.5
        };
        let mut available_ms = (effective_target - non_streaming).max(0.0) * fraction;
        if !self.startup_complete
            && !input.camera_moving
            && !input.camera_turning
            && self.motion_remaining_seconds <= 0.0
            && !self.recovery_reduced
        {
            // Startup spends a deliberate CPU allowance instead of inheriting
            // the small walking headroom of a naturally slow renderer. Queue
            // pressure still pauses intake while funded work drains; timing or
            // memory pressure retains the reduced budgets and service floors.
            available_ms = available_ms.max(positive_or(settings.target_frame_ms, 16.67) * 0.5);
        }
        let available_micros = (available_ms * 1_000.0) as u64;
        let sum = u128::from(budgets.max_commit_micros)
            + u128::from(budgets.max_activation_micros)
            + u128::from(budgets.max_collision_micros)
            + u128::from(budgets.max_admission_micros);
        let scale = if sum == 0 {
            0.0
        } else {
            (available_micros as f64 / sum as f64).min(1.0)
        };
        let minimum = settings.minimum.bounded_by(input.hard_limits);
        let allowance =
            |value: u64, floor: u64| ((value as f64 * scale) as u64).max(floor.min(value));
        budgets.max_commit_micros = allowance(budgets.max_commit_micros, minimum.max_commit_micros);
        budgets.max_activation_micros =
            allowance(budgets.max_activation_micros, minimum.max_activation_micros);
        budgets.max_collision_micros =
            allowance(budgets.max_collision_micros, minimum.max_collision_micros);
        budgets.max_admission_micros =
            allowance(budgets.max_admission_micros, minimum.max_admission_micros);
        // Finite service floors let already-dispatched work drain even when the
        // base frame itself exceeds target. They are not a frame-time guarantee.
        let actual = u128::from(budgets.max_commit_micros)
            + u128::from(budgets.max_activation_micros)
            + u128::from(budgets.max_collision_micros)
            + u128::from(budgets.max_admission_micros);
        actual > u128::from(available_micros)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> ControllerInput {
        ControllerInput {
            delta_seconds: 0.05,
            frame_ms: Some(8.0),
            gpu_frame_ms: None,
            streaming_work_ms: 0.5,
            camera_moving: false,
            camera_turning: false,
            camera_discontinuity: false,
            useful_nearby_ready: false,
            demand_settled: false,
            backlog: DownstreamBacklog::default(),
            active_scene_jobs: 0,
            hard_limits: ControllerSettings::default().startup,
            memory_blocked: false,
            mandatory_collision_pending: false,
        }
    }

    fn adaptive() -> ControllerSettings {
        ControllerSettings {
            adaptive: true,
            ..Default::default()
        }
    }

    fn advance(
        state: &mut ControllerState,
        settings: &ControllerSettings,
        input: ControllerInput,
        frames: usize,
    ) -> ControllerDecision {
        let mut decision = state.tick(settings, input);
        for _ in 1..frames {
            decision = state.tick(settings, input);
        }
        decision
    }

    fn assert_bounded(budgets: StageBudgets, hard: StageBudgets) {
        assert_eq!(budgets, budgets.bounded_by(hard));
    }

    #[test]
    fn fixed_backpressure_does_not_change_budgets_for_slow_rendering() {
        let settings = ControllerSettings::default();
        let input = ControllerInput {
            frame_ms: Some(120.0),
            ..input()
        };
        let decision = ControllerState::default().tick(&settings, input);
        assert_eq!(decision.mode, ControllerMode::Fixed);
        assert_eq!(
            decision.budgets,
            settings.fixed.bounded_by(input.hard_limits)
        );
        assert!(!decision.reasons.frame_pressure);
        assert!(decision.allow_scene_intake);
    }

    #[test]
    fn high_watermark_pauses_intake_without_stopping_dispatched_work() {
        let settings = ControllerSettings::default();
        let input = ControllerInput {
            backlog: DownstreamBacklog {
                ready_placements: settings.high_watermarks.ready_placements,
                ..Default::default()
            },
            ..input()
        };
        let decision = ControllerState::default().tick(&settings, input);
        assert_eq!(decision.mode, ControllerMode::Recovery);
        assert!(decision.reasons.queue_pressure);
        assert!(!decision.allow_scene_intake);
        assert!(!decision.allow_cell_requests);
        assert_eq!(decision.budgets.max_scene_jobs, 0);
        assert!(decision.budgets.max_cell_commits > 0);
        assert!(decision.budgets.max_model_activations > 0);
        assert!(decision.budgets.max_upload_bytes_per_frame > 0);
    }

    #[test]
    fn queue_pressure_retains_healthy_activation_and_upload_throughput() {
        for settings in [ControllerSettings::default(), adaptive()] {
            let input = ControllerInput {
                backlog: DownstreamBacklog {
                    ready_placements: settings.high_watermarks.ready_placements,
                    ..Default::default()
                },
                ..input()
            };
            let decision = ControllerState::default().tick(&settings, input);
            assert_eq!(decision.mode, ControllerMode::Recovery);
            assert!(!decision.allow_scene_intake);
            assert!(
                decision.budgets.max_model_activations > settings.minimum.max_model_activations
            );
            assert!(
                decision.budgets.max_upload_bytes_per_frame
                    > settings.minimum.max_upload_bytes_per_frame
            );
        }
    }

    #[test]
    fn timing_recovery_retains_reduced_drain_budgets_through_the_hold() {
        let settings = adaptive();
        let mut state = ControllerState {
            startup_complete: true,
            ..Default::default()
        };
        let base = ControllerInput {
            frame_ms: Some(40.0),
            streaming_work_ms: 1.0,
            ..input()
        };
        advance(&mut state, &settings, base, 20);
        state.tick(
            &settings,
            ControllerInput {
                streaming_work_ms: 10.0,
                ..base
            },
        );
        let holding = state.tick(&settings, base);
        assert_eq!(holding.mode, ControllerMode::Recovery);
        assert!(holding.reasons.recovery_hold);
        assert_eq!(
            holding.budgets.max_model_activations,
            settings.minimum.max_model_activations
        );
    }

    #[test]
    fn timing_and_memory_recover_drain_rate_while_large_queue_keeps_intake_paused() {
        for memory_pressure in [false, true] {
            let settings = adaptive();
            let mut state = ControllerState::default();
            let base = ControllerInput {
                frame_ms: Some(40.0),
                streaming_work_ms: 1.0,
                ..input()
            };
            advance(&mut state, &settings, base, 20);
            let queued = ControllerInput {
                backlog: DownstreamBacklog {
                    ready_placements: 2_000,
                    ..Default::default()
                },
                ..base
            };
            let spike = state.tick(
                &settings,
                ControllerInput {
                    frame_ms: Some(if memory_pressure { 40.0 } else { 50.0 }),
                    streaming_work_ms: if memory_pressure { 1.0 } else { 20.0 },
                    memory_blocked: memory_pressure,
                    ..queued
                },
            );
            assert_eq!(
                spike.budgets.max_model_activations,
                settings.minimum.max_model_activations
            );
            let holding = advance(&mut state, &settings, queued, 9);
            assert_eq!(
                holding.budgets.max_model_activations,
                settings.minimum.max_model_activations
            );
            let recovered = state.tick(&settings, queued);
            assert_eq!(recovered.mode, ControllerMode::Recovery);
            assert!(recovered.reasons.queue_pressure);
            assert!(!recovered.reasons.frame_pressure);
            assert!(!recovered.reasons.memory_pressure);
            assert!(!recovered.allow_scene_intake);
            assert!(!recovered.allow_cell_requests);
            assert_eq!(recovered.budgets.max_scene_jobs, 0);
            let healthy = settings.startup.bounded_by(queued.hard_limits);
            assert_eq!(
                recovered.budgets.max_model_activations,
                healthy.max_model_activations
            );
            assert_eq!(
                recovered.budgets.max_upload_bytes_per_frame,
                healthy.max_upload_bytes_per_frame
            );
            assert!(
                recovered.budgets.max_activation_micros > settings.minimum.max_activation_micros
            );
            assert_bounded(recovered.budgets, queued.hard_limits);
        }
    }

    #[test]
    fn recovery_requires_low_watermarks_and_a_stable_hold() {
        let settings = ControllerSettings::default();
        let mut state = ControllerState::default();
        let mut input = input();
        input.backlog.gpu_assets = settings.high_watermarks.gpu_assets;
        state.tick(&settings, input);
        input.backlog.gpu_assets = settings.low_watermarks.gpu_assets + 1;
        assert_eq!(
            advance(&mut state, &settings, input, 100).mode,
            ControllerMode::Recovery
        );
        input.backlog.gpu_assets = settings.low_watermarks.gpu_assets;
        assert_eq!(
            advance(&mut state, &settings, input, 5).mode,
            ControllerMode::Recovery
        );
        // A renewed high queue resets the recovery hold.
        input.backlog.gpu_assets = settings.high_watermarks.gpu_assets;
        state.tick(&settings, input);
        input.backlog.gpu_assets = 0;
        assert_eq!(
            advance(&mut state, &settings, input, 5).mode,
            ControllerMode::Recovery
        );
        assert_eq!(
            advance(&mut state, &settings, input, 7).mode,
            ControllerMode::Fixed
        );
    }

    #[test]
    fn every_downstream_queue_can_apply_backpressure() {
        let settings = ControllerSettings::default();
        let high = settings.high_watermarks;
        for backlog in [
            DownstreamBacklog {
                ready_placements: high.ready_placements,
                ..Default::default()
            },
            DownstreamBacklog {
                spawned_instances: high.spawned_instances,
                ..Default::default()
            },
            DownstreamBacklog {
                collider_jobs: high.collider_jobs,
                ..Default::default()
            },
            DownstreamBacklog {
                gpu_assets: high.gpu_assets,
                ..Default::default()
            },
            DownstreamBacklog {
                gpu_bytes: high.gpu_bytes,
                ..Default::default()
            },
            DownstreamBacklog {
                cell_responses: high.cell_responses,
                ..Default::default()
            },
        ] {
            let decision =
                ControllerState::default().tick(&settings, ControllerInput { backlog, ..input() });
            assert!(decision.reasons.queue_pressure);
            assert!(!decision.allow_scene_intake);
        }
    }

    #[test]
    fn zero_watermarks_disable_unavailable_observations() {
        let settings = ControllerSettings {
            high_watermarks: DownstreamBacklog::default(),
            ..Default::default()
        };
        let decision = ControllerState::default().tick(&settings, input());
        assert_eq!(decision.mode, ControllerMode::Fixed);
    }

    #[test]
    fn healthy_startup_uses_finite_burst_budgets_immediately() {
        let settings = adaptive();
        let decision = ControllerState::default().tick(&settings, input());
        assert_eq!(decision.mode, ControllerMode::Startup);
        assert_eq!(
            decision.budgets.max_scene_jobs,
            settings.startup.max_scene_jobs
        );
        assert_eq!(
            decision.budgets.max_model_activations,
            settings.startup.max_model_activations
        );
        assert_bounded(decision.budgets, input().hard_limits);
    }

    #[test]
    fn walking_or_turning_immediately_uses_cruise_and_holds_it() {
        let settings = adaptive();
        for turning in [false, true] {
            let mut state = ControllerState::default();
            state.tick(&settings, input());
            let mut input = input();
            input.camera_moving = !turning;
            input.camera_turning = turning;
            let decision = state.tick(&settings, input);
            assert_eq!(decision.mode, ControllerMode::Cruise);
            assert!(decision.budgets.max_scene_jobs <= settings.cruise.max_scene_jobs);
            input.camera_moving = false;
            input.camera_turning = false;
            assert_eq!(
                advance(&mut state, &settings, input, 3).mode,
                ControllerMode::Cruise
            );
            assert_eq!(
                advance(&mut state, &settings, input, 6).mode,
                ControllerMode::Startup
            );
        }
    }

    #[test]
    fn startup_exits_on_useful_readiness_or_settled_demand_and_never_restarts() {
        let settings = adaptive();
        for settled in [false, true] {
            let mut state = ControllerState::default();
            let not_ready = advance(&mut state, &settings, input(), 100);
            assert_eq!(not_ready.mode, ControllerMode::Startup);
            let ready = ControllerInput {
                useful_nearby_ready: !settled,
                demand_settled: settled,
                ..input()
            };
            assert!(!advance(&mut state, &settings, ready, 3).startup_complete);
            assert!(advance(&mut state, &settings, ready, 3).startup_complete);
            assert_ne!(state.tick(&settings, input()).mode, ControllerMode::Startup);
        }
    }

    #[test]
    fn quiet_budgets_ramp_and_camera_discontinuity_resets_the_ramp() {
        let settings = adaptive();
        let mut state = ControllerState::default();
        let ready = ControllerInput {
            demand_settled: true,
            ..input()
        };
        advance(&mut state, &settings, ready, 6);
        let early = advance(&mut state, &settings, ready, 12);
        let later = advance(&mut state, &settings, ready, 50);
        assert_eq!(later.mode, ControllerMode::Quiet);
        assert!(later.budgets.max_scene_jobs > early.budgets.max_scene_jobs);
        let teleported = state.tick(
            &settings,
            ControllerInput {
                camera_discontinuity: true,
                ..ready
            },
        );
        assert_eq!(teleported.mode, ControllerMode::Cruise);
        assert!(teleported.startup_complete);
        assert_eq!(teleported.quiet_seconds, 0.0);
        assert!(teleported.budgets.max_scene_jobs < later.budgets.max_scene_jobs);
        let after = advance(&mut state, &settings, ready, 70);
        assert_eq!(after.mode, ControllerMode::Quiet);
        assert!(after.budgets.max_scene_jobs > teleported.budgets.max_scene_jobs);
    }

    #[test]
    fn gpu_bound_baseline_continues_loading_and_cpu_streaming_spikes_back_off() {
        let settings = adaptive();
        let mut state = ControllerState::default();
        let slow_base = ControllerInput {
            frame_ms: Some(40.0),
            gpu_frame_ms: Some(40.0),
            streaming_work_ms: 1.0,
            ..input()
        };
        let decision = advance(&mut state, &settings, slow_base, 100);
        assert_eq!(decision.mode, ControllerMode::Startup);
        assert!(decision.allow_scene_intake);
        assert!(decision.budgets.max_scene_jobs > settings.minimum.max_scene_jobs);
        assert_eq!(decision.configured_target_frame_ms, 16.67);
        assert_eq!(decision.effective_target_frame_ms, 44.0);
        let spike = state.tick(
            &settings,
            ControllerInput {
                frame_ms: Some(50.0),
                streaming_work_ms: 10.0,
                camera_moving: true,
                ..slow_base
            },
        );
        assert_eq!(spike.mode, ControllerMode::Recovery);
        assert!(spike.reasons.frame_pressure);
        assert!(!spike.allow_scene_intake);
        let recovered = advance(&mut state, &settings, slow_base, 30);
        assert_ne!(recovered.mode, ControllerMode::Recovery);
        assert!(recovered.allow_scene_intake);
        assert!(recovered.budgets.max_scene_jobs < decision.budgets.max_scene_jobs);
    }

    #[test]
    fn stationary_startup_permits_cpu_bursts_but_still_pauses_overload() {
        let settings = adaptive();
        let mut state = ControllerState::default();
        for cpu in [9.0, 10.0, 12.0].into_iter().cycle().take(60) {
            let decision = state.tick(
                &settings,
                ControllerInput {
                    frame_ms: Some(16.0),
                    streaming_work_ms: cpu,
                    ..input()
                },
            );
            assert_eq!(decision.mode, ControllerMode::Startup);
            assert!(!decision.reasons.frame_pressure);
            assert!(decision.allow_scene_intake);
            assert!(
                decision.budgets.max_model_activations > settings.minimum.max_model_activations
            );
            assert!(
                decision.budgets.max_activation_micros > settings.minimum.max_activation_micros
            );
            assert_bounded(decision.budgets, input().hard_limits);
        }
        let overloaded = state.tick(
            &settings,
            ControllerInput {
                frame_ms: Some(20.0),
                streaming_work_ms: 20.0,
                ..input()
            },
        );
        assert!(overloaded.reasons.frame_pressure);
        assert_eq!(overloaded.mode, ControllerMode::Recovery);
        assert!(!overloaded.allow_scene_intake);
        assert_eq!(
            overloaded.budgets.max_model_activations,
            settings.minimum.max_model_activations
        );
    }

    #[test]
    fn slow_stationary_startup_keeps_useful_allowances_while_walking_uses_headroom() {
        let settings = adaptive();
        let base = ControllerInput {
            delta_seconds: 0.04,
            frame_ms: Some(40.0),
            streaming_work_ms: 1.0,
            ..input()
        };
        let mut state = ControllerState::default();
        let startup = advance(&mut state, &settings, base, 100);
        assert_eq!(startup.mode, ControllerMode::Startup);
        assert!(startup.allow_scene_intake);
        assert!(startup.budgets.max_activation_micros >= 1_700);
        assert!(startup.budgets.max_admission_micros >= 850);
        assert_bounded(startup.budgets, base.hard_limits);

        let mut completed = state.clone();
        completed.startup_complete = true;
        completed.quiet_seconds = 0.0;
        let later = completed.tick(&settings, base);
        assert_eq!(later.mode, ControllerMode::Cruise);
        assert!(later.budgets.max_activation_micros <= settings.cruise.max_activation_micros);
        assert!(later.budgets.max_admission_micros <= settings.cruise.max_admission_micros);

        let walking = state.tick(
            &settings,
            ControllerInput {
                camera_moving: true,
                ..base
            },
        );
        assert_eq!(walking.mode, ControllerMode::Cruise);
        assert!(walking.budgets.max_activation_micros <= settings.cruise.max_activation_micros);
        assert!(walking.budgets.max_admission_micros <= settings.cruise.max_admission_micros);
        assert!(walking.budgets.max_activation_micros < startup.budgets.max_activation_micros);
        assert_bounded(
            walking.budgets,
            settings.cruise.bounded_by(base.hard_limits),
        );

        // The motion hold retains walking allowance after releasing the keys.
        let held = state.tick(&settings, base);
        assert_eq!(held.mode, ControllerMode::Cruise);
        assert!(held.budgets.max_activation_micros <= settings.cruise.max_activation_micros);
        assert!(held.budgets.max_admission_micros <= settings.cruise.max_admission_micros);
    }

    #[test]
    fn queue_only_startup_recovery_keeps_drain_allowance_but_real_pressure_reduces_it() {
        let settings = adaptive();
        let base = ControllerInput {
            delta_seconds: 0.04,
            frame_ms: Some(40.0),
            streaming_work_ms: 1.0,
            ..input()
        };
        let mut state = ControllerState::default();
        advance(&mut state, &settings, base, 100);
        let queued = ControllerInput {
            backlog: DownstreamBacklog {
                ready_placements: 2_000,
                ..Default::default()
            },
            ..base
        };
        let draining = state.tick(&settings, queued);
        assert_eq!(draining.mode, ControllerMode::Recovery);
        assert!(draining.reasons.queue_pressure);
        assert!(!draining.allow_scene_intake);
        assert_eq!(draining.budgets.max_scene_jobs, 0);
        assert!(draining.budgets.max_activation_micros >= 1_700);
        assert!(draining.budgets.max_model_activations > settings.minimum.max_model_activations);
        assert_bounded(draining.budgets, queued.hard_limits);

        for memory in [false, true] {
            let mut pressured = state.clone();
            let reduced = pressured.tick(
                &settings,
                ControllerInput {
                    memory_blocked: memory,
                    streaming_work_ms: if memory { 1.0 } else { 20.0 },
                    ..queued
                },
            );
            assert!(!reduced.allow_scene_intake);
            assert_eq!(
                reduced.budgets.max_activation_micros,
                settings.minimum.max_activation_micros
            );
            assert_eq!(
                reduced.budgets.max_admission_micros,
                settings.minimum.max_admission_micros
            );
            assert_eq!(
                reduced.budgets.max_model_activations,
                settings.minimum.max_model_activations
            );
            assert_bounded(reduced.budgets, queued.hard_limits);
            let held = pressured.tick(&settings, queued);
            assert!(held.reasons.recovery_hold || held.reasons.queue_pressure);
            assert_eq!(
                held.budgets.max_activation_micros,
                settings.minimum.max_activation_micros
            );
        }
    }

    #[test]
    fn moving_during_startup_immediately_uses_walking_pressure_and_budgets() {
        let settings = adaptive();
        for turning in [false, true] {
            let mut state = ControllerState::default();
            let walking = ControllerInput {
                camera_moving: !turning,
                camera_turning: turning,
                frame_ms: Some(16.0),
                ..input()
            };
            let cruising = state.tick(&settings, walking);
            assert_eq!(cruising.mode, ControllerMode::Cruise);
            assert!(!cruising.startup_complete);
            assert_bounded(
                cruising.budgets,
                settings.cruise.bounded_by(walking.hard_limits),
            );
            let overloaded = state.tick(
                &settings,
                ControllerInput {
                    streaming_work_ms: 10.0,
                    ..walking
                },
            );
            assert!(overloaded.reasons.frame_pressure);
            assert_eq!(overloaded.mode, ControllerMode::Recovery);
            assert!(!overloaded.allow_scene_intake);
            assert_bounded(
                overloaded.budgets,
                settings.cruise.bounded_by(walking.hard_limits),
            );
            // Releasing the keys does not immediately restore the startup burst.
            let holding_motion = state.tick(
                &settings,
                ControllerInput {
                    camera_moving: false,
                    camera_turning: false,
                    streaming_work_ms: 10.0,
                    ..walking
                },
            );
            assert!(holding_motion.reasons.camera_motion);
            assert!(holding_motion.reasons.frame_pressure);
            assert_bounded(
                holding_motion.budgets,
                settings.cruise.bounded_by(walking.hard_limits),
            );
        }
    }

    #[test]
    fn completed_startup_retains_walking_cpu_pressure_while_stationary() {
        let settings = adaptive();
        let mut state = ControllerState {
            startup_complete: true,
            ..Default::default()
        };
        let decision = state.tick(
            &settings,
            ControllerInput {
                frame_ms: Some(16.0),
                streaming_work_ms: 10.0,
                ..input()
            },
        );
        assert!(decision.reasons.frame_pressure);
        assert_eq!(decision.mode, ControllerMode::Recovery);
        assert!(!decision.allow_scene_intake);
    }

    #[test]
    fn native_trace_recovers_with_normal_jitter_and_bounded_polling_cost() {
        let settings = adaptive();
        let mut state = ControllerState {
            frame_ema_ms: Some(19.231078141268565),
            non_streaming_ema_ms: Some(16.805507489096115),
            streaming_work_ema_ms: Some(2.4255706521724405),
            baseline_samples: VecDeque::from([(0.0, 6.333832)]),
            recovery: true,
            recovery_reduced: true,
            startup_complete: true,
            ..Default::default()
        };
        // Failed native smoke, frames 1762–1792: elapsed frame and previous
        // streaming CPU milliseconds reconstructed from its timing EMA. All
        // entry-pressure reasons were clear, yet the old exit gate never opened.
        let timings = [
            (17.6978, 2.4415),
            (18.1445, 2.3952),
            (24.4346, 2.3365),
            (11.3818, 2.4204),
            (19.6626, 2.3162),
            (17.5788, 2.3358),
            (19.2758, 2.1977),
            (18.3962, 2.4936),
            (18.2861, 2.3522),
            (17.9286, 2.4937),
            (18.4000, 2.2888),
            (22.0772, 2.4366),
            (14.9222, 2.2101),
            (18.5972, 2.3210),
            (17.6566, 2.7631),
            (18.7916, 2.5555),
            (18.1265, 2.7901),
            (18.5720, 2.7952),
            (19.4494, 2.4526),
            (16.7922, 2.6980),
            (19.7054, 2.3155),
            (17.0557, 2.7390),
            (21.7204, 2.3406),
            (15.8492, 2.4850),
            (17.1208, 2.7943),
            (18.8397, 2.3100),
            (17.9731, 2.2691),
            (17.8804, 2.4529),
            (19.8229, 2.3197),
            (18.8261, 2.2725),
            (18.5040, 2.4289),
        ];
        let mut decision = None;
        for (frame_ms, streaming_work_ms) in timings {
            let next = state.tick(
                &settings,
                ControllerInput {
                    delta_seconds: frame_ms / 1_000.0,
                    frame_ms: Some(frame_ms),
                    streaming_work_ms,
                    ..input()
                },
            );
            assert!(!next.reasons.frame_pressure);
            assert!(!next.reasons.frame_spike);
            decision = Some(next);
        }
        let recovered = decision.unwrap();
        assert_ne!(recovered.mode, ControllerMode::Recovery);
        assert!(recovered.allow_scene_intake);
        assert!(!recovered.reasons.recovery_probe);
        assert!(recovered.frame_ema_ms.unwrap() > settings.target_frame_ms * 0.9);

        // Safe ongoing polling can exceed the old 15%-of-target exit threshold.
        for index in 0..100 {
            let decision = state.tick(
                &settings,
                ControllerInput {
                    frame_ms: Some(if index % 2 == 0 { 18.9 } else { 19.1 }),
                    streaming_work_ms: if index % 2 == 0 { 2.6 } else { 2.7 },
                    ..input()
                },
            );
            assert!(decision.allow_scene_intake);
            assert!(!decision.reasons.recovery_probe);
            assert_bounded(decision.budgets, input().hard_limits);
        }
    }

    #[test]
    fn native_variable_frames_do_not_repeatedly_pause_safe_streaming() {
        let settings = adaptive();
        // Corrected native smoke, frames 801–864. Ordinary presentation variance
        // and 1–3 ms polling repeatedly restarted recovery in the failed run.
        let timings = [
            (27.5524, 1.1706),
            (10.1675, 1.2842),
            (19.9291, 1.1132),
            (19.5076, 1.2662),
            (18.8772, 1.4083),
            (19.3578, 1.2049),
            (20.3557, 1.2935),
            (21.4543, 1.4669),
            (14.7773, 1.2550),
            (31.3561, 1.1993),
            (9.1606, 1.1841),
            (17.8850, 1.6206),
            (19.1965, 1.2208),
            (22.1490, 1.2372),
            (18.2336, 1.2005),
            (17.0462, 1.3611),
            (20.4678, 1.1996),
            (17.6575, 1.6430),
            (18.9934, 1.2427),
            (19.0863, 1.1622),
            (19.2258, 1.1272),
            (21.0422, 1.3079),
            (17.6693, 1.5982),
            (18.1223, 1.1899),
            (19.0905, 1.1320),
            (19.0838, 1.1963),
            (21.4451, 1.1094),
            (18.0709, 1.3257),
            (19.7039, 1.1242),
            (19.0338, 1.1641),
            (19.4765, 1.1807),
            (19.0295, 1.3051),
            (20.2239, 1.1760),
            (24.7824, 1.4229),
            (10.4640, 1.1511),
            (20.5132, 1.1802),
            (20.6176, 1.2673),
            (18.4386, 2.8950),
            (19.5679, 2.8898),
            (19.8725, 3.0453),
            (24.9081, 3.0392),
            (11.0269, 1.6588),
            (22.2500, 1.3771),
            (17.8849, 1.7510),
            (19.6785, 1.6782),
            (18.6036, 1.6212),
            (19.3309, 1.6638),
            (20.1075, 1.5067),
            (18.1092, 1.6849),
            (19.1169, 1.5627),
            (18.9250, 1.3932),
            (19.6439, 1.4390),
            (20.3955, 1.5413),
            (17.7151, 1.6892),
            (19.8846, 1.3598),
            (17.8529, 1.6242),
            (20.6674, 1.3191),
            (19.2535, 1.4479),
            (18.6773, 1.4479),
            (26.9276, 1.3203),
            (9.6728, 1.8154),
            (21.7242, 1.3539),
            (20.3706, 1.4414),
            (17.3369, 1.8119),
        ];
        let mut state = ControllerState {
            frame_ema_ms: Some(29.143965249070565),
            non_streaming_ema_ms: Some(27.820484405261347),
            streaming_work_ema_ms: Some(1.3234808438092187),
            baseline_samples: timings
                .iter()
                .map(|(frame, cpu)| (0.0, frame - cpu))
                .collect(),
            recovery: true,
            recovery_reduced: true,
            startup_complete: true,
            ..Default::default()
        };
        for index in 0..timings.len() * 3 {
            let (frame_ms, streaming_work_ms) = timings[index % timings.len()];
            let decision = state.tick(
                &settings,
                ControllerInput {
                    delta_seconds: frame_ms / 1_000.0,
                    frame_ms: Some(frame_ms),
                    streaming_work_ms,
                    ..input()
                },
            );
            assert!(!decision.reasons.frame_pressure, "sample {index}");
            assert!(!decision.reasons.frame_spike, "sample {index}");
            if index >= 30 {
                assert!(decision.allow_scene_intake, "sample {index}");
                assert!(!decision.reasons.recovery_probe);
            }
            assert_bounded(decision.budgets, input().hard_limits);
        }
        // Neither presentation variance nor its wider spike reference can hide
        // expensive streaming CPU work or a severe unexplained frame stall.
        let cpu_spike = state.tick(
            &settings,
            ControllerInput {
                frame_ms: Some(30.0),
                streaming_work_ms: 10.0,
                ..input()
            },
        );
        assert!(cpu_spike.reasons.frame_pressure);
        assert!(!cpu_spike.allow_scene_intake);
        let frame_spike = state.tick(
            &settings,
            ControllerInput {
                frame_ms: Some(80.0),
                streaming_work_ms: 1.3,
                ..input()
            },
        );
        assert!(frame_spike.reasons.frame_spike);
        assert!(!frame_spike.allow_scene_intake);
    }

    #[test]
    fn startup_usefulness_waits_until_recovery_has_finished() {
        let settings = adaptive();
        let mut state = ControllerState::default();
        let ready = ControllerInput {
            useful_nearby_ready: true,
            ..input()
        };
        let blocked = advance(
            &mut state,
            &settings,
            ControllerInput {
                streaming_work_ms: 20.0,
                ..ready
            },
            30,
        );
        assert_eq!(blocked.mode, ControllerMode::Recovery);
        assert!(!blocked.startup_complete);
        let recovered = advance(&mut state, &settings, ready, 11);
        assert_eq!(recovered.mode, ControllerMode::Startup);
        assert!(!recovered.startup_complete);
        let useful = advance(&mut state, &settings, ready, 5);
        assert!(useful.startup_complete);
    }

    #[test]
    fn short_async_frames_do_not_pin_a_sustained_gpu_bound_baseline() {
        let settings = adaptive();
        let mut state = ControllerState::default();
        let base = ControllerInput {
            delta_seconds: 0.04,
            frame_ms: Some(40.0),
            streaming_work_ms: 2.6,
            ..input()
        };
        advance(&mut state, &settings, base, 60);
        let spike = state.tick(
            &settings,
            ControllerInput {
                frame_ms: Some(50.0),
                streaming_work_ms: 10.0,
                camera_moving: true,
                ..base
            },
        );
        assert_eq!(spike.mode, ControllerMode::Recovery);
        assert!(spike.reasons.frame_pressure);
        let frame_times = [40.0, 42.0, 38.0, 9.0, 40.0, 41.0, 39.0, 40.0];
        for index in 0..240 {
            let frame_ms = frame_times[index % frame_times.len()];
            let decision = state.tick(
                &settings,
                ControllerInput {
                    delta_seconds: frame_ms / 1_000.0,
                    frame_ms: Some(frame_ms),
                    ..base
                },
            );
            assert!(!decision.reasons.frame_pressure);
            assert!(!decision.reasons.frame_spike);
            assert!(decision.effective_target_frame_ms > 40.0);
            if index >= 20 {
                assert!(decision.allow_scene_intake);
                assert!(!decision.reasons.recovery_probe);
            }
            assert_bounded(decision.budgets, base.hard_limits);
        }
        let slow_frame = state.tick(
            &settings,
            ControllerInput {
                frame_ms: Some(80.0),
                ..base
            },
        );
        assert_eq!(slow_frame.mode, ControllerMode::Recovery);
        assert!(slow_frame.reasons.frame_spike);
        assert!(!slow_frame.allow_scene_intake);
    }

    #[test]
    fn large_relative_frame_spike_enters_recovery_without_reported_streaming_work() {
        let settings = adaptive();
        let mut state = ControllerState::default();
        advance(&mut state, &settings, input(), 10);
        let decision = state.tick(
            &settings,
            ControllerInput {
                frame_ms: Some(50.0),
                streaming_work_ms: 0.0,
                ..input()
            },
        );
        assert_eq!(decision.mode, ControllerMode::Recovery);
        assert!(decision.reasons.frame_spike);
        assert!(!decision.allow_scene_intake);
    }

    #[test]
    fn sustained_frame_pressure_has_bounded_progress_probes() {
        let settings = adaptive();
        let mut state = ControllerState {
            startup_complete: true,
            ..Default::default()
        };
        let input = ControllerInput {
            frame_ms: Some(40.0),
            streaming_work_ms: 10.0,
            active_scene_jobs: 3,
            ..input()
        };
        let mut probes = 0;
        for _ in 0..100 {
            let decision = state.tick(&settings, input);
            assert_eq!(decision.mode, ControllerMode::Recovery);
            if decision.reasons.recovery_probe {
                probes += 1;
                assert!(decision.allow_scene_intake);
                assert_eq!(decision.budgets.max_scene_jobs, input.active_scene_jobs + 1);
            } else {
                assert!(!decision.allow_scene_intake);
            }
        }
        assert_eq!(probes, 2);
    }

    #[test]
    fn memory_and_queue_pressure_block_progress_probes() {
        let settings = adaptive();
        for memory in [false, true] {
            let mut state = ControllerState::default();
            let mut input = input();
            input.memory_blocked = memory;
            if !memory {
                input.backlog.collider_jobs = settings.high_watermarks.collider_jobs;
            }
            for _ in 0..200 {
                let decision = state.tick(&settings, input);
                assert!(!decision.reasons.recovery_probe);
                assert!(!decision.allow_scene_intake);
            }
        }
    }

    #[test]
    fn collision_floor_respects_remaining_hard_job_cap_and_memory() {
        let settings = adaptive();
        for (active, cap, memory, expected) in [
            (3, 4, false, 1),
            (4, 4, false, 0),
            (5, 4, false, 0),
            (3, 4, true, 0),
        ] {
            let mut input = input();
            input.backlog.ready_placements = settings.high_watermarks.ready_placements;
            input.mandatory_collision_pending = true;
            input.active_scene_jobs = active;
            input.hard_limits.max_scene_jobs = cap;
            input.memory_blocked = memory;
            let decision = ControllerState::default().tick(&settings, input);
            assert_eq!(decision.mandatory_scene_jobs, expected);
            assert!(decision.mandatory_model_activations <= decision.budgets.max_model_activations);
        }
    }

    #[test]
    fn every_effective_budget_respects_hard_limits_even_service_floors() {
        let settings = adaptive();
        let hard = StageBudgets {
            max_scene_jobs: 0,
            max_model_activations: 0,
            max_cell_commits: 0,
            max_upload_bytes_per_frame: 0,
            max_commit_micros: 0,
            max_activation_micros: 0,
            max_collision_micros: 0,
            max_admission_micros: 0,
        };
        for pressure in [false, true] {
            let input = ControllerInput {
                hard_limits: hard,
                memory_blocked: pressure,
                mandatory_collision_pending: true,
                ..input()
            };
            let decision = ControllerState::default().tick(&settings, input);
            assert_eq!(decision.budgets, hard);
            assert!(!decision.allow_scene_intake);
            assert!(!decision.allow_cell_requests);
            assert_eq!(decision.mandatory_scene_jobs, 0);
            assert_eq!(decision.mandatory_model_activations, 0);
        }
        let tiny = StageBudgets {
            max_scene_jobs: 1,
            max_model_activations: 1,
            max_cell_commits: 1,
            max_upload_bytes_per_frame: 1,
            max_commit_micros: 1,
            max_activation_micros: 1,
            max_collision_micros: 1,
            max_admission_micros: 1,
        };
        let decision = ControllerState::default().tick(
            &settings,
            ControllerInput {
                hard_limits: tiny,
                ..input()
            },
        );
        assert_bounded(decision.budgets, tiny);
    }

    #[test]
    fn invalid_timings_are_ignored_and_long_stalls_do_not_complete_quiet_holds() {
        let settings = adaptive();
        let mut state = ControllerState::default();
        let decision = state.tick(
            &settings,
            ControllerInput {
                frame_ms: Some(f64::NAN),
                gpu_frame_ms: Some(f64::INFINITY),
                delta_seconds: f64::NAN,
                streaming_work_ms: f64::NAN,
                ..input()
            },
        );
        assert_eq!(decision.frame_ema_ms, None);
        assert_eq!(decision.gpu_frame_ema_ms, None);
        assert_eq!(decision.quiet_seconds, 0.0);
        assert_bounded(decision.budgets, input().hard_limits);
        let decision = state.tick(
            &settings,
            ControllerInput {
                delta_seconds: 10.0,
                ..input()
            },
        );
        assert!(decision.quiet_seconds <= 0.25);
    }

    #[test]
    fn stale_gpu_samples_expire_instead_of_holding_recovery_forever() {
        let settings = adaptive();
        let mut state = ControllerState::default();
        let initial = ControllerInput {
            gpu_frame_ms: Some(80.0),
            ..input()
        };
        state.tick(&settings, initial);
        let decision = advance(&mut state, &settings, input(), 60);
        assert_eq!(decision.gpu_frame_ema_ms, None);
        assert_ne!(decision.mode, ControllerMode::Recovery);
    }
}
