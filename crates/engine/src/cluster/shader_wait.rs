//! Terminal handling for failed or indefinitely unavailable required compute shaders.
use super::*;
use bevy::{render::render_resource::CachedPipelineState, shader::ShaderCacheError};
use std::time::{Duration, Instant};

// Loading/compiling this fixed shader set is independent of scene size. Two minutes
// permits a cold driver compile while ensuring a missing embedded shader cannot
// leave a requested scene waiting forever. This is a failure deadline, not a draw cap.
const SHADER_WAIT_LIMIT: Duration = Duration::from_secs(120);

#[derive(Default)]
struct ShaderWait {
    finished: bool,
}

impl ShaderWait {
    fn observe(
        &mut self,
        elapsed: Duration,
        failure: Option<String>,
        pending: Option<&str>,
    ) -> Option<String> {
        if self.finished {
            return None;
        }
        if let Some(reason) = failure {
            self.finished = true;
            return Some(reason);
        }
        if let Some(name) = pending {
            if elapsed >= SHADER_WAIT_LIMIT {
                self.finished = true;
                return Some(format!(
                    "cluster {name} shader unavailable after {} seconds",
                    SHADER_WAIT_LIMIT.as_secs()
                ));
            }
            return None;
        }
        self.finished = true;
        None
    }
}

pub(super) struct Watch {
    source_tick: Option<bevy::ecs::change_detection::Tick>,
    started: Instant,
    wait: ShaderWait,
}

impl Default for Watch {
    fn default() -> Self {
        Self {
            source_tick: None,
            started: Instant::now(),
            wait: Default::default(),
        }
    }
}

pub(super) fn monitor(
    scene: Option<Res<ClusterScene>>,
    gpu: Option<Res<ClusterGpu>>,
    frame: Res<ClusterFrame>,
    pipelines: Res<ClusterPipelines>,
    cache: Res<PipelineCache>,
    failures: Res<UploadFailures>,
    mut watch: Local<Watch>,
) {
    let (Some(scene), Some(_gpu), Some(source_tick)) = (scene, gpu, frame.scene_tick) else {
        return;
    };
    if watch.source_tick != Some(source_tick) {
        *watch = Watch {
            source_tick: Some(source_tick),
            ..Default::default()
        };
    }
    if watch.wait.finished {
        return;
    }
    let mut pending = None;
    let mut failure = None;
    for (name, id) in [
        ("instance cull", pipelines.instance_cull),
        ("cluster cull", pipelines.cull),
        ("instance compaction", pipelines.compaction[0]),
        ("instance scan", pipelines.compaction[1]),
        ("instance scatter", pipelines.compaction[2]),
        ("cluster compaction", pipelines.compaction[3]),
        ("cluster scan", pipelines.compaction[4]),
        ("cluster scatter", pipelines.compaction[5]),
        ("origin shift", pipelines.shift),
    ] {
        let Some(id) = id else {
            pending.get_or_insert(name);
            continue;
        };
        match cache.get_compute_pipeline_state(id) {
            CachedPipelineState::Ok(_) => {}
            CachedPipelineState::Err(
                ShaderCacheError::ShaderNotLoaded(_)
                | ShaderCacheError::ShaderImportNotYetAvailable,
            )
            | CachedPipelineState::Queued
            | CachedPipelineState::Creating(_) => {
                pending.get_or_insert(name);
            }
            CachedPipelineState::Err(error) => {
                failure = Some(format!("cluster {name} shader failed: {error}"));
                break;
            }
        }
    }
    let elapsed = watch.started.elapsed();
    if let Some(reason) = watch.wait.observe(elapsed, failure, pending) {
        failures.report(scene.ready_revision.clone(), source_tick, reason);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_permanently_unavailable_shader_is_a_terminal_failure() {
        let mut wait = ShaderWait::default();
        assert!(
            wait.observe(Duration::ZERO, None, Some("cluster cull"))
                .is_none()
        );
        assert!(
            wait.observe(
                SHADER_WAIT_LIMIT - Duration::from_millis(1),
                None,
                Some("cluster cull")
            )
            .is_none()
        );
        let reason = wait
            .observe(SHADER_WAIT_LIMIT, None, Some("cluster cull"))
            .expect("a missing shader must not hold readiness forever");
        assert!(reason.contains("cluster cull"));
        assert!(wait.finished);
        assert!(
            wait.observe(SHADER_WAIT_LIMIT * 2, None, Some("cluster cull"))
                .is_none()
        );
    }

    #[test]
    fn a_failed_shader_reports_its_exact_reason_without_waiting_for_the_deadline() {
        let mut wait = ShaderWait::default();
        let reason = "cluster cull shader failed: invalid binding".to_owned();
        assert_eq!(
            wait.observe(Duration::ZERO, Some(reason.clone()), None),
            Some(reason)
        );
        assert!(wait.finished);
    }

    #[test]
    fn successful_shader_preparation_settles_before_the_deadline() {
        let mut wait = ShaderWait::default();
        assert!(
            wait.observe(Duration::ZERO, None, Some("cluster cull"))
                .is_none()
        );
        assert!(!wait.finished);
        assert!(wait.observe(Duration::from_secs(1), None, None).is_none());
        assert!(wait.finished);
    }
}
