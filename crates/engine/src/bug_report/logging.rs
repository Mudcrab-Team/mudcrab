//! Bounded, opt-in diagnostic capture alongside Bevy's normal logging layers.
//!
//! This layer does not install a subscriber, read `RUST_LOG`, or change filtering.
//! The report exporter must sanitize paths and credential patterns before writing
//! the returned text. Arbitrary log messages still require player review.

use std::{
    cell::Cell,
    collections::VecDeque,
    fmt::{self, Write as _},
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use bevy::{
    app::App,
    ecs::resource::Resource,
    log::{
        BoxedLayer,
        tracing_subscriber::{Layer, layer::Context},
    },
};
use tracing::{
    Event, Level, Subscriber,
    field::{Field, Visit},
};

const MAX_LOG_EVENTS: usize = 2_048;
const MAX_LOG_BYTES: usize = 1_048_576;
const MAX_EVENT_BYTES: usize = 4_096;
const MAX_EVENT_FIELDS: usize = 64;
const MAX_METADATA_BYTES: usize = 1_024;

/// Recent diagnostics shared by the subscriber and the reporting service.
#[derive(Resource, Clone, Default)]
pub struct RecentLogs {
    inner: Arc<Mutex<LogRing>>,
}

impl RecentLogs {
    /// Returns at most 1 MiB of valid UTF-8, including range and omission counts.
    ///
    /// Only events delivered by the existing subscriber's filters are considered.
    /// Counts cover this buffer's lifetime, while the range covers retained events.
    /// Paths, ANSI escapes, control characters, and message-level secrets must be
    /// sanitized by the common exporter before this text leaves the process.
    pub fn snapshot(&self) -> String {
        let ring = self.lock();
        let mut output = BoundedText::new(MAX_LOG_BYTES);
        let _ = writeln!(output, "Recent engine diagnostics");
        let _ = writeln!(
            output,
            "Timestamp unit: milliseconds since Unix epoch (UTC)"
        );
        let _ = write!(output, "Represented range: ");
        write_timestamp(
            &mut output,
            ring.events.front().and_then(|event| event.timestamp_ms),
        );
        let _ = write!(output, " .. ");
        write_timestamp(
            &mut output,
            ring.events.back().and_then(|event| event.timestamp_ms),
        );
        let _ = writeln!(output);
        let _ = writeln!(
            output,
            "retained_events={} captured_events={} dropped_events={} truncated_events={} omitted_events={} omitted_fields={}",
            ring.events.len(),
            ring.captured_events,
            ring.dropped_events,
            ring.truncated_events,
            ring.omitted_events,
            ring.omitted_fields,
        );
        let _ = writeln!(output);
        // The ring includes its entry allocation in its byte budget, leaving much
        // more room than the bounded header needs in this exported text buffer.
        for event in &ring.events {
            if output.write_str(&event.text).is_err() {
                break;
            }
        }
        output.text
    }

    fn lock(&self) -> MutexGuard<'_, LogRing> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn omit_event(&self) {
        let mut ring = self.lock();
        ring.omitted_events = ring.omitted_events.saturating_add(1);
    }
}

/// Adds a bounded collection layer without replacing Bevy's terminal logger.
pub fn report_log_layer(app: &mut App) -> Option<BoxedLayer> {
    if app
        .world()
        .get_resource::<crate::config::EngineConfig>()
        .is_some_and(|config| {
            config.is_benchmark_run() || config.benchmark_only || config.shots.is_some()
        })
    {
        return None;
    }
    let logs = app
        .world()
        .get_resource::<RecentLogs>()
        .cloned()
        .unwrap_or_default();
    app.insert_resource(logs.clone());
    Some(Box::new(ReportLogLayer { logs }))
}

struct LogEvent {
    timestamp_ms: Option<u128>,
    text: Box<str>,
}

struct LogRing {
    events: VecDeque<LogEvent>,
    text_bytes: usize,
    captured_events: u64,
    dropped_events: u64,
    truncated_events: u64,
    omitted_events: u64,
    omitted_fields: u64,
}

impl Default for LogRing {
    fn default() -> Self {
        Self {
            events: VecDeque::with_capacity(MAX_LOG_EVENTS),
            text_bytes: 0,
            captured_events: 0,
            dropped_events: 0,
            truncated_events: 0,
            omitted_events: 0,
            omitted_fields: 0,
        }
    }
}

impl LogRing {
    fn owned_bytes(&self) -> usize {
        // Include the fixed ring allocation and Arc/Mutex storage, not only text.
        std::mem::size_of::<Mutex<Self>>()
            + 2 * std::mem::size_of::<usize>()
            + self.events.capacity() * std::mem::size_of::<LogEvent>()
            + self.text_bytes
    }

    fn push(&mut self, event: LogEvent, truncated: bool, omitted_fields: u64) {
        self.captured_events = self.captured_events.saturating_add(1);
        self.truncated_events = self.truncated_events.saturating_add(u64::from(truncated));
        self.omitted_fields = self.omitted_fields.saturating_add(omitted_fields);
        while self.events.len() >= MAX_LOG_EVENTS
            || self.owned_bytes().saturating_add(event.text.len()) > MAX_LOG_BYTES
        {
            let Some(oldest) = self.events.pop_front() else {
                self.omitted_events = self.omitted_events.saturating_add(1);
                return;
            };
            self.text_bytes -= oldest.text.len();
            self.dropped_events = self.dropped_events.saturating_add(1);
        }
        self.text_bytes += event.text.len();
        self.events.push_back(event);
    }
}

struct ReportLogLayer {
    logs: RecentLogs,
}

thread_local! {
    // A user-provided Debug implementation can itself emit a tracing event.
    // Suppress recursive collection and never hold the ring lock while formatting.
    static FORMATTING_EVENT: Cell<bool> = const { Cell::new(false) };
}

struct FormattingGuard;

impl Drop for FormattingGuard {
    fn drop(&mut self) {
        FORMATTING_EVENT.with(|active| active.set(false));
    }
}

impl<S: Subscriber> Layer<S> for ReportLogLayer {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        let metadata = event.metadata();
        if !matches!(*metadata.level(), Level::INFO | Level::WARN | Level::ERROR) {
            return;
        }
        // Inspect names before any values are formatted. Sensitive fields omit
        // the whole event, including its message, rather than merely hiding a key.
        if !allowed_target(metadata.target())
            || metadata.target().len() > MAX_METADATA_BYTES
            || metadata.fields().len() > MAX_EVENT_FIELDS
            || event.fields().any(|field| {
                field.name().len() > MAX_METADATA_BYTES || forbidden_field(field.name())
            })
        {
            self.logs.omit_event();
            return;
        }
        if FORMATTING_EVENT.with(|active| active.replace(true)) {
            self.logs.omit_event();
            return;
        }
        let _guard = FormattingGuard;
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|time| time.as_millis());
        // Reserve the final newline within the 4 KiB event cap.
        let mut visitor = EventVisitor {
            output: BoundedText {
                text: String::with_capacity(MAX_EVENT_BYTES),
                limit: MAX_EVENT_BYTES - 1,
                truncated: false,
            },
            omitted_fields: 0,
        };
        let _ = write!(visitor.output, "[unix_ms=");
        write_timestamp(&mut visitor.output, timestamp_ms);
        let _ = write!(
            visitor.output,
            "] {} {}: ",
            metadata.level(),
            metadata.target()
        );
        event.record(&mut visitor);
        let truncated = visitor.output.truncated;
        visitor.output.text.push('\n');
        let entry = LogEvent {
            timestamp_ms,
            text: visitor.output.text.into_boxed_str(),
        };
        self.logs
            .lock()
            .push(entry, truncated, visitor.omitted_fields);
    }
}

fn allowed_target(target: &str) -> bool {
    // Engine entry points that can contain raw console/CLI history (console,
    // config, main, shots) are deliberately absent. New targets require review.
    const TARGETS: &[&str] = &[
        "engine::app",
        "engine::color_pipeline",
        "engine::lights",
        "engine::mesh_preparation_retry",
        "engine::metrics",
        "engine::nif_material",
        "engine::pacing",
        "engine::physics",
        "engine::prepass_vertex_alpha",
        "engine::profiling",
        "engine::render",
        "engine::renderer_init",
        "engine::render_timing",
        "engine::schedule_timing",
        "engine::sky",
        "engine::streaming",
        "engine::world",
        "bevy_asset",
        "bevy_image",
        "bevy_render",
        "bevy_pbr",
        "bevy_core_pipeline",
        "wgpu",
        "wgpu_core",
        "wgpu_hal",
        "naga",
    ];
    TARGETS.iter().any(|prefix| {
        target == *prefix
            || target
                .strip_prefix(prefix)
                .is_some_and(|suffix| suffix.starts_with("::"))
    })
}

fn forbidden_field(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "password",
        "passwd",
        "secret",
        "token",
        "credential",
        "cookie",
        "authorization",
        "api_key",
        "access_key",
        "private_key",
        "session_key",
    ]
    .iter()
    .any(|needle| name.contains(needle))
        || matches!(
            name.as_str(),
            "args"
                | "arguments"
                | "argv"
                | "cli"
                | "cli_args"
                | "raw_cli"
                | "raw_command"
                | "command_args"
                | "command"
                | "command_line"
                | "commandline"
                | "console_history"
                | "console_input"
                | "console_command"
                | "history"
                | "environment"
                | "env"
                | "config"
                | "engine_config"
                | "headers"
        )
}

fn reviewed_field(name: &str) -> bool {
    matches!(
        name,
        "message"
            | "error"
            | "reason"
            | "path"
            | "model"
            | "asset"
            | "asset_id"
            | "assets"
            | "cell"
            | "reference"
            | "base"
            | "worldspace"
            | "grid"
            | "grid_x"
            | "grid_y"
            | "camera"
            | "target"
            | "ground_height"
            | "frame"
            | "render_frame"
            | "entity"
            | "key"
            | "neighbor"
            | "side"
            | "max_delta"
            | "chain"
            | "backlog"
            | "bound"
            | "threads"
            | "existing"
            | "instances"
            | "pending"
            | "pending_peak"
            | "resumed"
            | "canceled"
            | "candidates"
            | "newly_tracked"
            | "duplicate_roots"
            | "orphaned_roots"
            | "missing_roots"
            | "out_of_range_roots"
            | "elapsed_millis"
            | "position"
            | "count"
            | "size"
            | "bytes"
            | "duration"
            | "backend"
            | "adapter"
            | "format"
            | "width"
            | "height"
            | "index"
            | "id"
    )
}

struct EventVisitor {
    output: BoundedText,
    omitted_fields: u64,
}

impl EventVisitor {
    fn record(&mut self, field: &Field, value: fmt::Arguments<'_>) {
        if !reviewed_field(field.name()) {
            self.omitted_fields = self.omitted_fields.saturating_add(1);
            return;
        }
        if self.output.truncated {
            return;
        }
        if field.name() != "message" {
            let _ = write!(self.output, " {}=", field.name());
        }
        let _ = self.output.write_fmt(value);
    }
}

impl Visit for EventVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.record(field, format_args!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.record(field, format_args!("{value}"));
    }

    fn record_bytes(&mut self, _field: &Field, _value: &[u8]) {
        // Raw binary payloads are never part of a diagnostic text report.
        self.omitted_fields = self.omitted_fields.saturating_add(1);
    }
}

/// Bounds allocation while Debug/Display executes, rather than allocating an
/// unbounded formatted string and truncating it afterwards.
struct BoundedText {
    text: String,
    limit: usize,
    truncated: bool,
}

impl BoundedText {
    fn new(limit: usize) -> Self {
        Self {
            text: String::with_capacity(limit),
            limit,
            truncated: false,
        }
    }
}

impl fmt::Write for BoundedText {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        if self.truncated {
            return Err(fmt::Error);
        }
        let available = self.limit.saturating_sub(self.text.len());
        if value.len() <= available {
            self.text.push_str(value);
            return Ok(());
        }
        let mut boundary = available;
        while !value.is_char_boundary(boundary) {
            boundary -= 1;
        }
        self.text.push_str(&value[..boundary]);
        self.truncated = true;
        Err(fmt::Error)
    }
}

fn write_timestamp(output: &mut BoundedText, timestamp: Option<u128>) {
    match timestamp {
        Some(timestamp) => {
            let _ = write!(output, "{timestamp}");
        }
        None => {
            let _ = output.write_str("unavailable");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::log::tracing_subscriber::{EnvFilter, Registry, prelude::*};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn collect(events: impl FnOnce()) -> RecentLogs {
        let logs = RecentLogs::default();
        let subscriber = Registry::default().with(ReportLogLayer { logs: logs.clone() });
        tracing::subscriber::with_default(subscriber, events);
        logs
    }

    #[test]
    fn keeps_diagnostic_levels_and_reviewed_targets() {
        let logs = collect(|| {
            tracing::info!(target: "engine::streaming", cell = 42, "visible info");
            tracing::warn!(target: "bevy_asset::server", "visible warning");
            tracing::error!(target: "wgpu_core::device", "visible error");
            tracing::debug!(target: "engine::streaming", "verbose debug");
            tracing::info!(target: "unrelated_app", "unrelated application");
            tracing::info!(target: "engine::console", "raw console input");
            tracing::info!(target: "engine::streaming_credentials", "prefix lookalike");
        });
        let snapshot = logs.snapshot();
        assert!(snapshot.contains("visible info"));
        assert!(snapshot.contains("cell=42"));
        assert!(snapshot.contains("visible warning"));
        assert!(snapshot.contains("visible error"));
        assert!(!snapshot.contains("verbose debug"));
        assert!(!snapshot.contains("unrelated application"));
        assert!(!snapshot.contains("raw console input"));
        assert!(!snapshot.contains("prefix lookalike"));
        assert!(snapshot.contains("omitted_events=3"));
    }

    struct MustNotFormat;

    impl fmt::Debug for MustNotFormat {
        fn fmt(&self, _output: &mut fmt::Formatter<'_>) -> fmt::Result {
            panic!("an excluded field must not be formatted");
        }
    }

    #[test]
    fn omits_sensitive_events_and_never_formats_unknown_fields() {
        let logs = collect(|| {
            tracing::info!(target: "engine::streaming", password = ?MustNotFormat, "secret event");
            tracing::warn!(target: "engine::streaming", access_token = ?MustNotFormat, "token event");
            tracing::error!(target: "engine::app", argv = ?MustNotFormat, "command event");
            tracing::info!(target: "engine::streaming", unreviewed_object = ?MustNotFormat, "safe message");
            tracing::info!(target: "engine::streaming", asset = &b"raw binary payload"[..], "binary field omitted");
        });
        let snapshot = logs.snapshot();
        assert!(!snapshot.contains("secret event"));
        assert!(!snapshot.contains("token event"));
        assert!(!snapshot.contains("command event"));
        assert!(snapshot.contains("safe message"));
        assert!(snapshot.contains("binary field omitted"));
        assert!(!snapshot.contains("raw binary payload"));
        assert!(snapshot.contains("omitted_events=3 omitted_fields=2"));
    }

    #[test]
    fn respects_existing_subscriber_filter() {
        let logs = RecentLogs::default();
        let subscriber = Registry::default()
            .with(ReportLogLayer { logs: logs.clone() })
            .with(EnvFilter::new("warn"));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "engine::streaming", "filtered info");
            tracing::warn!(target: "engine::streaming", "permitted warning");
        });
        let snapshot = logs.snapshot();
        assert!(!snapshot.contains("filtered info"));
        assert!(snapshot.contains("permitted warning"));
        assert!(snapshot.contains("captured_events=1"));
    }

    #[test]
    fn collection_allowlist_does_not_filter_other_logging_layers() {
        struct OtherLogger(Arc<AtomicUsize>);
        impl<S: Subscriber> Layer<S> for OtherLogger {
            fn on_event(&self, _event: &Event<'_>, _context: Context<'_, S>) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }

        let observed = Arc::new(AtomicUsize::new(0));
        let logs = RecentLogs::default();
        let subscriber = Registry::default()
            .with(ReportLogLayer { logs: logs.clone() })
            .with(OtherLogger(observed.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "unrelated_app", "normal terminal log");
            tracing::debug!(target: "engine::streaming", "normal verbose log");
        });
        assert_eq!(observed.load(Ordering::Relaxed), 2);
        assert_eq!(logs.lock().captured_events, 0);
    }

    #[test]
    fn evicts_oldest_events_at_the_count_limit() {
        let logs = collect(|| {
            for index in 0..2_500 {
                tracing::info!(target: "engine::streaming", "event-{index:04}");
            }
        });
        let snapshot = logs.snapshot();
        assert!(snapshot.contains("retained_events=2048 captured_events=2500 dropped_events=452"));
        assert!(!snapshot.contains("event-0451"));
        assert!(snapshot.contains("event-0452"));
        assert!(snapshot.contains("event-2499"));
        assert!(logs.lock().owned_bytes() <= MAX_LOG_BYTES);
    }

    struct LongDebug<'a>(&'a AtomicUsize);

    impl fmt::Debug for LongDebug<'_> {
        fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
            for _ in 0..1_000_000 {
                self.0.fetch_add(1, Ordering::Relaxed);
                output.write_str("🦀")?;
            }
            Ok(())
        }
    }

    #[test]
    fn bounds_unicode_during_debug_formatting() {
        let writes = AtomicUsize::new(0);
        let logs = collect(|| {
            tracing::warn!(target: "engine::streaming", reason = ?LongDebug(&writes), "large event");
        });
        let ring = logs.lock();
        assert_eq!(ring.truncated_events, 1);
        assert!(ring.events[0].text.len() <= MAX_EVENT_BYTES);
        assert!(std::str::from_utf8(ring.events[0].text.as_bytes()).is_ok());
        assert!(writes.load(Ordering::Relaxed) <= MAX_EVENT_BYTES / 4 + 1);
        drop(ring);
        assert!(logs.snapshot().contains("truncated_events=1"));
    }

    #[test]
    fn caps_memory_and_snapshot_and_preserves_the_latest_event() {
        let payload = "🦀".repeat(2_000);
        let logs = collect(|| {
            for index in 0..1_000 {
                tracing::error!(target: "engine::streaming", index, reason = %payload, "overflow");
            }
        });
        let ring = logs.lock();
        assert!(ring.owned_bytes() <= MAX_LOG_BYTES);
        assert!(ring.events.len() < MAX_LOG_EVENTS);
        assert!(ring.dropped_events > 0);
        assert_eq!(ring.truncated_events, 1_000);
        assert!(ring.events.back().unwrap().text.contains("index=999"));
        drop(ring);
        let snapshot = logs.snapshot();
        assert!(snapshot.len() <= MAX_LOG_BYTES);
        assert!(snapshot.contains("Represented range:"));
        assert!(snapshot.contains("index=999"));
        assert!(std::str::from_utf8(snapshot.as_bytes()).is_ok());
    }

    struct RecursiveDebug;

    impl fmt::Debug for RecursiveDebug {
        fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
            tracing::warn!(target: "engine::streaming", "nested diagnostic");
            output.write_str("outer diagnostic")
        }
    }

    #[test]
    fn formatting_can_log_without_recursion_or_a_deadlock() {
        let logs = collect(|| {
            tracing::info!(target: "engine::streaming", reason = ?RecursiveDebug, "outer event");
            tracing::info!(target: "engine::streaming", "following event");
        });
        let snapshot = logs.snapshot();
        assert!(snapshot.contains("outer diagnostic"));
        assert!(snapshot.contains("following event"));
        assert!(!snapshot.contains("nested diagnostic"));
    }

    #[test]
    fn custom_layer_reuses_the_apps_existing_buffer() {
        let mut app = App::new();
        let logs = collect(|| tracing::info!(target: "engine::streaming", "existing event"));
        app.insert_resource(logs);
        assert!(report_log_layer(&mut app).is_some());
        assert!(
            app.world()
                .resource::<RecentLogs>()
                .snapshot()
                .contains("existing event")
        );
    }

    #[test]
    fn automated_benchmarks_and_shots_do_not_install_collection() {
        use crate::config::EngineConfig;

        for config in [
            EngineConfig {
                benchmark_frames: Some(1),
                ..Default::default()
            },
            EngineConfig {
                benchmark_only: true,
                ..Default::default()
            },
            EngineConfig {
                shots: Some("shots.json".into()),
                ..Default::default()
            },
        ] {
            let mut app = App::new();
            app.insert_resource(config);
            assert!(report_log_layer(&mut app).is_none());
            assert!(!app.world().contains_resource::<RecentLogs>());
        }
    }
}
