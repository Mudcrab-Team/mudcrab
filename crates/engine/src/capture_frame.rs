//! Receipts tied to screenshot entities, rather than callback arrival frames.
//!
//! Bevy 0.19 extracts each new Screenshot once and marks it Capturing in that same
//! extraction. A ticket is sampled in that extraction and finalized after rendering;
//! ScreenshotCaptured for the identical entity certifies completion of its readback.
//! Automated runners serialize primary-window requests and keep the pose frozen.
use crate::{
    color_pipeline::{
        SceneGradeMode, SceneGradeOverrides, SceneGradeSource, SceneVisualPreset,
        SkyrimCinematicPost,
    },
    shots::shot_camera_translation,
    streaming::RenderOrigin,
    tone_mapping::SceneToneMapSource,
    world::components::StreamingCamera,
};
use bevy::{
    camera::{CompositingSpace, Exposure},
    core_pipeline::tonemapping::Tonemapping,
    diagnostic::FrameCount,
    prelude::*,
    render::{
        Extract, ExtractSchedule, Render, RenderApp, RenderSystems,
        mesh::RenderMesh,
        render_asset::{ExtractedAssets, RenderAssets, prepare_assets},
        texture::GpuImage,
        view::ColorGrading,
        view::screenshot::Screenshot,
    },
};
use serde::Serialize;
use std::sync::{Arc, Mutex};

#[derive(Clone, Component, Default)]
pub(crate) struct CaptureTicket {
    receipt: Arc<Mutex<Option<CaptureFrame>>>,
    /// The run's failure counters when this shot started settling, frozen into the request so the receipt
    /// reports only the failures of this shot: an earlier shot's failures are history, and one
    /// that happens after the request still shows up in the receipt.
    baseline: crate::shots::Failures,
    /// Stage 3 probes read from the same frame, when the request names any.
    probes: Option<crate::hdr_probe::ProbeTicket>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct CaptureFrame {
    pub main_frame: u32,
    pub render_frame: u64,
    pub camera_creation: [f32; 3],
    pub rotation_runtime_xyzw: [f32; 4],
    pub vertical_fov_radians: Option<f32>,
    pub render_origin: [i32; 2],
    pub pending_specializations: Option<usize>,
    pub work: crate::shots::SettleCounts,
    pub terrain_coverage: Vec<serde_json::Value>,
    pub terrain_gauges: std::collections::BTreeMap<String, f64>,
    /// Absent only in harnesses that build a world without the engine's startup resources.
    pub visual_profile: Option<crate::visual_profile::VisualProfile>,
    pub grading: Option<CaptureGrade>,
    /// Tuning values sampled in the same extraction as the photographed frame.
    pub grass_tuning: Option<serde_json::Value>,
    pub grass_snow_gate: bool,
}

/// Immutable grading evidence sampled with the rendered camera, before readback.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct CaptureGrade {
    pub color_grade: String,
    pub visual_preset: String,
    pub preset_saturation: f32,
    pub resolved_grade: serde_json::Value,
    pub source: serde_json::Value,
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn capture_grade(
    source: Option<&SceneGradeSource>,
    tone_source: Option<&SceneToneMapSource>,
    mode: Option<&SceneGradeMode>,
    preset: Option<&SceneVisualPreset>,
    grading: Option<&ColorGrading>,
    post: Option<&SkyrimCinematicPost>,
    tonemapping: Option<&Tonemapping>,
    exposure: Option<&Exposure>,
    compositing: Option<&CompositingSpace>,
) -> Option<CaptureGrade> {
    capture_grade_with_overrides(
        source,
        tone_source,
        mode,
        preset,
        grading,
        post,
        tonemapping,
        exposure,
        compositing,
        SceneGradeOverrides::default(),
    )
}

#[allow(clippy::too_many_arguments)]
fn capture_grade_with_overrides(
    source: Option<&SceneGradeSource>,
    tone_source: Option<&SceneToneMapSource>,
    mode: Option<&SceneGradeMode>,
    preset: Option<&SceneVisualPreset>,
    grading: Option<&ColorGrading>,
    post: Option<&SkyrimCinematicPost>,
    tonemapping: Option<&Tonemapping>,
    exposure: Option<&Exposure>,
    compositing: Option<&CompositingSpace>,
    overrides: SceneGradeOverrides,
) -> Option<CaptureGrade> {
    let (source, mode, preset, grading, post) = (source?, *mode?, *preset?, grading?, post?);
    let fallback = SceneToneMapSource::default();
    let tone = tone_source.unwrap_or(&fallback);
    let resolved = tone
        .resolve_with_overrides(source, mode, preset, overrides)
        .ok()?;
    let (tonemapping, exposure, compositing) = (tonemapping?, exposure?, compositing?);
    // A source describes this frame only after its settings reached the camera.
    if grading.global.post_saturation != resolved.bevy_saturation
        || *post != resolved.post
        || *tonemapping != resolved.tonemapping
        || *compositing != CompositingSpace::Linear
    {
        return None;
    }
    let grade = resolved.grade;
    let mut evidence = tone.evidence_with_overrides(source, mode, preset, overrides);
    evidence["exposure_ev100"] = exposure.ev100.into();
    evidence["actual_camera"] = serde_json::json!({
        "tonemapping": format!("{tonemapping:?}"),
        "exposure_ev100": exposure.ev100,
        "compositing_space": format!("{compositing:?}"),
    });
    Some(CaptureGrade {
        color_grade: mode.name().into(),
        visual_preset: preset.name().into(),
        preset_saturation: preset.grade().saturation,
        resolved_grade: serde_json::json!({
            "saturation": grade.saturation,
            "brightness": grade.brightness,
            "contrast": grade.contrast,
            "tint_amount": grade.tint_amount,
            "tint_rgb": grade.tint_rgb,
            "bevy_saturation": grading.global.post_saturation,
            "post_saturation": post.saturation,
            "post_linear_domain": post.linear_domain,
            "post_enabled": post.enabled,
            "pivot": post.pivot,
            "tone_operator": post.tone_operator,
            "tone_coefficient": post.tone_coefficient,
            "sampled_luminance": post.sampled_luminance,
            "target_luminance": post.target_luminance,
        }),
        source: evidence,
    })
}

impl CaptureTicket {
    /// A ticket carrying the failure baseline of the shot that requested it.
    pub(crate) fn new(baseline: crate::shots::Failures) -> Self {
        Self {
            receipt: Arc::new(Mutex::new(None)),
            baseline,
            probes: None,
        }
    }

    /// Also samples these points before tone mapping in the captured frame.
    pub(crate) fn with_probes(mut self, points: Vec<crate::hdr_probe::ProbePoint>) -> Self {
        if !points.is_empty() {
            self.probes = Some(crate::hdr_probe::ProbeTicket::new(points));
        }
        self
    }

    pub(crate) fn probes(&self) -> Option<&crate::hdr_probe::ProbeTicket> {
        self.probes.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn baseline(&self) -> crate::shots::Failures {
        self.baseline
    }

    pub(crate) fn receipt(&self) -> Option<CaptureFrame> {
        self.receipt.lock().unwrap().clone()
    }

    #[cfg(test)]
    pub(crate) fn set_receipt(&self, frame: CaptureFrame) {
        *self.receipt.lock().unwrap() = Some(frame);
    }
}

pub(crate) struct CaptureFramePlugin;
impl Plugin for CaptureFramePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RetryReadiness>()
            .init_resource::<TransferReadiness>()
            .add_plugins(crate::hdr_probe::HdrProbePlugin);
        let bridge = app.world().resource::<RetryReadiness>().clone();
        let transfers = app.world().resource::<TransferReadiness>().clone();
        if let Some(render) = app.get_sub_app_mut(RenderApp) {
            render
                .insert_resource(bridge)
                .insert_resource(transfers)
                .init_resource::<PendingRenderTransfers>()
                .init_resource::<ExtractedCaptures>()
                .add_systems(ExtractSchedule, extract_capture_frames)
                .add_systems(
                    Render,
                    (
                        observe_render_transfers
                            .before(prepare_assets::<RenderMesh>)
                            .before(prepare_assets::<GpuImage>),
                        publish_render_transfers
                            .after(RenderSystems::Queue)
                            .before(RenderSystems::Cleanup),
                        finalize_capture_frames.in_set(RenderSystems::Cleanup),
                    ),
                );
        }
    }
}

/// Updated after specialization/queue observation, and explicitly unavailable until observed.
#[derive(Resource, Clone, Default)]
pub(crate) struct RetryReadiness(pub Arc<Mutex<Option<usize>>>);

#[derive(Resource, Default)]
struct ExtractedCaptures {
    render_frame: u64,
    captures: Vec<(CaptureTicket, CaptureFrame)>,
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
/// Freeze the rendered frame's pose, appearance and current work, including enabled grass.
fn extract_capture_frames(
    mut extracted: ResMut<ExtractedCaptures>,
    frame_inputs: Extract<(
        Res<FrameCount>,
        Res<RenderOrigin>,
        Query<
            (
                &Transform,
                &Projection,
                Option<&ColorGrading>,
                Option<&SkyrimCinematicPost>,
                Option<&Tonemapping>,
                Option<&Exposure>,
                Option<&CompositingSpace>,
            ),
            With<StreamingCamera>,
        >,
        Query<&CaptureTicket, With<Screenshot>>,
        Res<crate::streaming::StreamingMetrics>,
        Res<crate::render::RendererMetrics>,
        Res<crate::profiling::ProfilingState>,
        Res<crate::terrain_upload::TerrainMeshUploadReadiness>,
        Option<Res<crate::config::EngineConfig>>,
        Option<Res<crate::grass::GrassMetrics>>,
        Option<Res<crate::grass_render::GrassRenderMetrics>>,
    )>,
    scene_inputs: Extract<(
        Query<Entity, With<bevy::window::PrimaryWindow>>,
        Query<(&crate::streaming::TerrainCoverage, &InheritedVisibility)>,
        Query<&crate::streaming::lod::terrain_batching::LodTerrainBatches>,
        Option<Res<SceneGradeSource>>,
        Option<Res<SceneGradeMode>>,
        Option<Res<SceneVisualPreset>>,
        Option<Res<SceneToneMapSource>>,
        Option<Res<SceneGradeOverrides>>,
        Option<Res<crate::visual_profile::VisualProfile>>,
        Option<Res<crate::grass_tuning::GrassTuningState>>,
    )>,
) {
    let (
        frame,
        origin,
        camera,
        tickets,
        streaming,
        renderer,
        profiler,
        uploads,
        config,
        grass,
        grass_renderer,
    ) = &*frame_inputs;
    let (
        windows,
        surfaces,
        batches,
        grade_source,
        grade_mode,
        visual_preset,
        tone_source,
        grade_overrides,
        visual_profile,
        grass_tuning,
    ) = &*scene_inputs;
    extracted.render_frame += 1;
    extracted.captures.clear();
    if windows.single().is_err() || tickets.iter().all(|ticket| ticket.receipt().is_some()) {
        return;
    }
    let Ok((transform, projection, grading, post, tonemapping, exposure, compositing)) =
        camera.single()
    else {
        return;
    };
    // Reuse the same origin conversion as the pose driver; origin is an absolute cell offset.
    let offset = shot_camera_translation([0.0; 3], origin.0);
    let absolute = transform.translation - offset;
    let pose = shared::coordinates::runtime_to_creation_vector(absolute.to_array());
    for ticket in tickets {
        if ticket.receipt().is_some() {
            continue;
        }
        let mut work = captured_work(
            ticket,
            streaming,
            renderer,
            profiler,
            uploads,
            renderer.final_path_active(),
        );
        work.read_grass(
            config.as_ref().is_some_and(|config| config.grass),
            grass.as_deref(),
            grass_renderer.as_deref(),
        );
        let record = CaptureFrame {
            main_frame: frame.0,
            render_frame: extracted.render_frame,
            camera_creation: pose,
            rotation_runtime_xyzw: transform.rotation.to_array(),
            vertical_fov_radians: match projection {
                Projection::Perspective(p) => Some(p.fov),
                _ => None,
            },
            render_origin: origin.0.to_array(),
            pending_specializations: None,
            work,
            visual_profile: visual_profile.as_deref().copied(),
            grass_tuning: grass_tuning.as_ref().map(|state| state.receipt()),
            grass_snow_gate: crate::grass::generation::MAJORITY_GRASS_SUPPORT,
            grading: capture_grade_with_overrides(
                grade_source.as_deref(),
                tone_source.as_deref(),
                grade_mode.as_deref(),
                visual_preset.as_deref(),
                grading,
                post,
                tonemapping,
                exposure,
                compositing,
                grade_overrides.as_deref().copied().unwrap_or_default(),
            ),
            terrain_gauges: profiler.terrain_gauges(),
            terrain_coverage: surfaces
                .iter()
                .filter(|(_, visible)| visible.get())
                .map(|(coverage, _)| serde_json::json!({"source": coverage.receipt()}))
                .chain(batches.iter().flat_map(|batch| batch.coverage_receipt()))
                .collect(),
        };
        extracted.captures.push((ticket.clone(), record));
    }
}

/// Base work for a captured receipt, read against the baseline frozen when its capture was
/// requested: a failure from an earlier shot is history and cannot reject this receipt, while a
/// failure after the request - including one between the request and this extraction - still can.
/// Everything pending is read as it stands, so work that is unavailable rather than absent leaves
/// the receipt without a claim of readiness.
/// Extraction adds grass from the same frame with `SettleCounts::read_grass`; its failures stay current.
fn captured_work(
    ticket: &CaptureTicket,
    streaming: &crate::streaming::StreamingMetrics,
    renderer: &crate::render::RendererMetrics,
    profiler: &crate::profiling::ProfilingState,
    uploads: &crate::terrain_upload::TerrainMeshUploadReadiness,
    renderer_ready: bool,
) -> crate::shots::SettleCounts {
    let mut work =
        crate::shots::SettleCounts::read(streaming, renderer, ticket.baseline, renderer_ready, 0);
    work.outstanding_terrain_uploads = uploads.outstanding();
    work.pending_batch_cpu = profiler
        .gauge("lod/pending_terrain_batch_chunks")
        .unwrap_or(0.0) as usize;
    work.pending_batch_initial = profiler
        .gauge("lod/pending_initial_terrain_upload_chunks")
        .unwrap_or(0.0) as usize;
    work.pending_batch_selection = profiler
        .gauge("lod/pending_terrain_selection_uploads")
        .unwrap_or(0.0) as usize;
    work
}

fn finalize_capture_frames(
    mut extracted: ResMut<ExtractedCaptures>,
    retries: Res<RetryReadiness>,
    transfers: Res<TransferReadiness>,
) {
    let pending = *retries.0.lock().unwrap();
    for (ticket, mut record) in extracted.captures.drain(..) {
        record.pending_specializations = pending;
        record.work.pending_specializations = pending;
        record.work.pending_render_transfers = *transfers.0.lock().unwrap();
        let mut receipt = ticket.receipt.lock().unwrap();
        // Never overwrite the render frame when asynchronous readback takes more frames.
        if receipt.is_none() {
            *receipt = Some(record);
        }
    }
}

pub(crate) fn validate_receipt(
    ticket: &CaptureTicket,
    requested: [f32; 3],
) -> Result<CaptureFrame, String> {
    let record = ticket.receipt().ok_or("no captured render-frame receipt")?;
    if record.camera_creation != requested {
        return Err("captured pose differs from requested pose".into());
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the real extraction inputs with all non-grass work ready, without a GPU.
    fn grass_capture_world(
        enabled: bool,
        cpu: Option<(bool, u64)>,
        renderer: Option<(bool, u64)>,
    ) -> (World, CaptureTicket) {
        let ticket = CaptureTicket::default();
        let mut main = bevy::render::MainWorld::default();
        main.insert_resource(FrameCount(7));
        main.insert_resource(RenderOrigin(IVec2::ZERO));
        main.insert_resource(crate::config::EngineConfig {
            grass: enabled,
            ..default()
        });
        main.insert_resource(crate::streaming::StreamingMetrics::default());
        main.insert_resource(crate::profiling::ProfilingState::default());
        main.insert_resource(crate::terrain_upload::TerrainMeshUploadReadiness::default());
        main.insert_resource(crate::render::RendererMetrics {
            gpu_preprocessing_active: true,
            gpu_culling_active: true,
            indirect_drawing_active: true,
            occlusion_culling_views: 1,
            hzb_views: 1,
            indirect_phase_buffers: 1,
            indirect_batch_sets: 1,
            proof_frames: 1,
            ..default()
        });
        if let Some((ready, failures)) = cpu {
            main.insert_resource(crate::grass::GrassMetrics {
                ready,
                failures,
                ..default()
            });
        }
        if let Some((ready, failures)) = renderer {
            main.insert_resource(crate::grass_render::GrassRenderMetrics { ready, failures });
        }
        main.spawn(bevy::window::PrimaryWindow);
        main.spawn((
            StreamingCamera,
            Transform::default(),
            Projection::Perspective(PerspectiveProjection::default()),
        ));
        main.spawn((Screenshot::primary_window(), ticket.clone()));

        let mut render = World::new();
        render.insert_resource(main);
        render.init_resource::<ExtractedCaptures>();
        render.insert_resource(RetryReadiness(Arc::new(Mutex::new(Some(0)))));
        render.insert_resource(TransferReadiness(Arc::new(Mutex::new(Some(0)))));
        let mut extraction = Schedule::new(ExtractSchedule);
        extraction.add_systems(extract_capture_frames);
        render.add_schedule(extraction);
        (render, ticket)
    }

    /// Finalize the extracted request through the production system and read its ticket.
    fn finish_grass_capture(render: &mut World, ticket: &CaptureTicket) -> CaptureFrame {
        use bevy::ecs::system::RunSystemOnce;
        render.run_system_once(finalize_capture_frames).unwrap();
        let receipt = validate_receipt(ticket, [0.0; 3]).unwrap();
        assert_eq!(receipt.main_frame, 7);
        assert_eq!(receipt.render_frame, 1);
        assert!(receipt.work.renderer_ready);
        assert_eq!(receipt.pending_specializations, Some(0));
        assert_eq!(receipt.work.pending_specializations, Some(0));
        assert_eq!(receipt.work.pending_render_transfers, Some(0));
        receipt
    }

    /// Either missing grass observer leaves the extracted frame pending, including both absent.
    #[test]
    fn g3_capture_receipt_missing_grass_metrics_is_pending() {
        for (cpu, renderer) in [
            (None, None),
            (Some((true, 0)), None),
            (None, Some((true, 0))),
        ] {
            let (mut render, ticket) = grass_capture_world(true, cpu, renderer);
            render.run_schedule(ExtractSchedule);
            let receipt = finish_grass_capture(&mut render, &ticket);
            assert!(
                receipt.work.grass_pending,
                "missing metrics cpu={cpu:?} renderer={renderer:?}: {}",
                receipt.work.describe()
            );
            assert_eq!(receipt.work.grass_failures, 0);
            assert!(!receipt.work.is_quiet(), "{}", receipt.work.describe());
        }
    }

    /// CPU generation and admitted-but-undrawn grass each prevent a quiet receipt.
    #[test]
    fn g3_capture_receipt_waits_for_cpu_and_render_grass() {
        for (cpu_ready, render_ready) in [(false, true), (true, false), (false, false)] {
            let (mut render, ticket) =
                grass_capture_world(true, Some((cpu_ready, 0)), Some((render_ready, 0)));
            render.run_schedule(ExtractSchedule);
            let receipt = finish_grass_capture(&mut render, &ticket);
            assert!(
                receipt.work.grass_pending,
                "CPU ready={cpu_ready}, render ready={render_ready}: {}",
                receipt.work.describe()
            );
            assert_eq!(receipt.work.grass_failures, 0);
            assert!(!receipt.work.is_quiet(), "{}", receipt.work.describe());
        }
    }

    /// Both grass observers ready and failure-free allow an otherwise settled frame.
    #[test]
    fn g3_capture_receipt_ready_grass_is_quiet() {
        let (mut render, ticket) = grass_capture_world(true, Some((true, 0)), Some((true, 0)));
        render.run_schedule(ExtractSchedule);
        let receipt = finish_grass_capture(&mut render, &ticket);
        assert!(!receipt.work.grass_pending);
        assert_eq!(receipt.work.grass_failures, 0);
        assert!(receipt.work.is_quiet(), "{}", receipt.work.describe());
    }

    /// Screenshot extraction samples the resource once; later live edits cannot relabel it.
    #[test]
    fn capture_receipt_freezes_tuning_resource_at_extraction() {
        let (mut render, ticket) = grass_capture_world(true, Some((true, 0)), Some((true, 0)));
        let mut tuning = crate::grass_tuning::GrassTuningState::new(None);
        tuning.effective.brightness = 0.625;
        let expected = tuning.receipt();
        render
            .resource_mut::<bevy::render::MainWorld>()
            .insert_resource(tuning);
        render.run_schedule(ExtractSchedule);
        render
            .resource_mut::<bevy::render::MainWorld>()
            .resource_mut::<crate::grass_tuning::GrassTuningState>()
            .effective
            .brightness = 1.75;
        let receipt = finish_grass_capture(&mut render, &ticket);
        assert_eq!(receipt.grass_tuning, Some(expected));
    }

    /// Independent CPU and render failures accumulate even when both report readiness.
    #[test]
    fn g3_capture_receipt_sums_grass_failures() {
        let (mut render, ticket) = grass_capture_world(true, Some((true, 2)), Some((true, 3)));
        render.run_schedule(ExtractSchedule);
        let receipt = finish_grass_capture(&mut render, &ticket);
        assert_eq!(
            receipt.work.grass_failures,
            5,
            "{}",
            receipt.work.describe()
        );
        assert!(!receipt.work.grass_pending);
        assert!(!receipt.work.is_quiet(), "{}", receipt.work.describe());
    }

    /// Disabled grass ignores missing observers and stale pending/failure metrics alike.
    #[test]
    fn g3_capture_receipt_disabled_grass_ignores_metrics() {
        for (cpu, renderer) in [(None, None), (Some((false, 2)), Some((false, 3)))] {
            let (mut render, ticket) = grass_capture_world(false, cpu, renderer);
            render.run_schedule(ExtractSchedule);
            let receipt = finish_grass_capture(&mut render, &ticket);
            assert!(!receipt.work.grass_pending);
            assert_eq!(receipt.work.grass_failures, 0);
            assert!(receipt.work.is_quiet(), "{}", receipt.work.describe());
        }
    }

    /// Finalization preserves extraction-time grass state despite subsequent main-world updates.
    #[test]
    fn g3_capture_receipt_freezes_grass_at_extraction() {
        for initially_ready in [false, true] {
            let (cpu_failures, render_failures, expected_failures) = if initially_ready {
                (0, 0, 0)
            } else {
                (2, 3, 5)
            };
            let (mut render, ticket) = grass_capture_world(
                true,
                Some((initially_ready, cpu_failures)),
                Some((initially_ready, render_failures)),
            );
            render.run_schedule(ExtractSchedule);
            {
                let mut main = render.resource_mut::<bevy::render::MainWorld>();
                main.resource_mut::<FrameCount>().0 = 99;
                main.insert_resource(crate::grass::GrassMetrics {
                    ready: !initially_ready,
                    failures: if initially_ready { 11 } else { 0 },
                    ..default()
                });
                main.insert_resource(crate::grass_render::GrassRenderMetrics {
                    ready: !initially_ready,
                    failures: if initially_ready { 13 } else { 0 },
                });
            }
            let receipt = finish_grass_capture(&mut render, &ticket);
            assert_eq!(receipt.work.grass_pending, !initially_ready);
            assert_eq!(receipt.work.grass_failures, expected_failures);
            assert_eq!(
                receipt.work.is_quiet(),
                initially_ready,
                "{}",
                receipt.work.describe()
            );
        }
    }

    #[test]
    fn incomplete_capture_has_no_receipt() {
        assert!(validate_receipt(&CaptureTicket::default(), [0.0; 3]).is_err());
    }

    #[test]
    fn tuned_camera_and_capture_evidence_share_effective_resolution() {
        let source = SceneGradeSource::default();
        let tone = SceneToneMapSource::default();
        let mode = SceneGradeMode::SnapshotSrgb;
        let preset = SceneVisualPreset::CoolGray;
        let overrides = SceneGradeOverrides {
            brightness: Some(1.74),
            tint_green: Some(0.67),
            pivot: Some(0.29),
            ..default()
        };
        let resolved = tone
            .resolve_with_overrides(&source, mode, preset, overrides)
            .unwrap();
        let mut grading = ColorGrading::default();
        grading.global.post_saturation = resolved.bevy_saturation;
        let exposure = Exposure {
            ev100: crate::color_pipeline::DEFAULT_SCENE_EV100,
        };
        let capture = capture_grade_with_overrides(
            Some(&source),
            Some(&tone),
            Some(&mode),
            Some(&preset),
            Some(&grading),
            Some(&resolved.post),
            Some(&resolved.tonemapping),
            Some(&exposure),
            Some(&CompositingSpace::Linear),
            overrides,
        )
        .expect("the same resolved grade labels the camera frame");
        assert_eq!(
            capture.resolved_grade["brightness"],
            serde_json::json!(1.74_f32)
        );
        assert_eq!(
            capture.resolved_grade["tint_rgb"][1],
            serde_json::json!(0.67_f32)
        );
        assert_eq!(capture.resolved_grade["pivot"], serde_json::json!(0.29_f32));
        assert_eq!(capture.source["pivot"], serde_json::json!(0.29_f32));
        assert_eq!(capture.source["default_pivot"], serde_json::json!(0.18_f32));
        assert_eq!(
            capture.source["grade_overrides"]["brightness"],
            serde_json::json!(1.74_f32)
        );
        assert_eq!(
            capture.source["effective_grade"]["brightness"],
            serde_json::json!(1.74_f32)
        );
        assert_eq!(
            capture.source["effective_stages"]["pivot"],
            serde_json::json!(0.29_f32)
        );
    }

    #[test]
    fn render_receipt_is_immutable_across_callback_delay() {
        let ticket = CaptureTicket::default();
        let source = SceneGradeSource::default();
        let mode = SceneGradeMode::SnapshotSrgb;
        let preset = SceneVisualPreset::CoolGray;
        let resolved = source.resolve(mode, preset).unwrap();
        let mut grading = ColorGrading::default();
        grading.global.post_saturation = resolved.bevy_saturation;
        let record = CaptureFrame {
            main_frame: 3,
            render_frame: 2,
            camera_creation: [1.0; 3],
            rotation_runtime_xyzw: [0.0; 4],
            vertical_fov_radians: None,
            render_origin: [0; 2],
            pending_specializations: Some(0),
            work: default(),
            terrain_gauges: default(),
            terrain_coverage: default(),
            visual_profile: None,
            grass_tuning: Some(
                serde_json::json!({"path": "first.toml", "sha256": "first", "effective": {"brightness": 0.6}}),
            ),
            grading: capture_grade(
                Some(&source),
                None,
                Some(&mode),
                Some(&preset),
                Some(&grading),
                Some(&resolved.post),
                Some(&Tonemapping::TonyMcMapface),
                Some(&Exposure { ev100: 9.7 }),
                Some(&CompositingSpace::Linear),
            ),
        };
        use bevy::ecs::system::RunSystemOnce;
        let mut world = World::new();
        world.insert_resource(ExtractedCaptures {
            render_frame: 2,
            captures: vec![(ticket.clone(), record.clone())],
        });
        world.insert_resource(RetryReadiness(Arc::new(Mutex::new(Some(0)))));
        world.insert_resource(TransferReadiness(Arc::new(Mutex::new(Some(0)))));
        world.run_system_once(finalize_capture_frames).unwrap();
        let mut later = record;
        later.main_frame = 99;
        later.render_frame = 98;
        later.grass_tuning = Some(
            serde_json::json!({"path": "later.toml", "sha256": "later", "effective": {"brightness": 1.0}}),
        );
        later.grading = Some(CaptureGrade {
            color_grade: "record-srgb".into(),
            visual_preset: "vivid".into(),
            preset_saturation: 1.5,
            resolved_grade: serde_json::json!({"brightness": 1.135}),
            source: serde_json::json!({"source_kind": "authored-image-space-record"}),
        });
        world
            .resource_mut::<ExtractedCaptures>()
            .captures
            .push((ticket.clone(), later));
        world.run_system_once(finalize_capture_frames).unwrap();
        let captured = validate_receipt(&ticket, [1.0; 3]).unwrap();
        assert_eq!(captured.render_frame, 2);
        assert_eq!(
            captured.grass_tuning.unwrap()["effective"]["brightness"],
            0.6
        );
        let captured_grade = captured.grading.unwrap();
        assert_eq!(captured_grade.color_grade, "snapshot-srgb");
        assert_eq!(captured_grade.visual_preset, "cool-gray");
        assert_eq!(
            captured_grade.resolved_grade["brightness"].as_f64(),
            Some(f64::from(1.38_f32))
        );
        assert_eq!(
            captured_grade.source["source_kind"],
            "approved-artistic-override"
        );
        assert!(validate_receipt(&ticket, [0.0; 3]).is_err());
    }

    /// A receipt is checked against the failures of the shot it belongs to: the run's history is
    /// not this capture's rejection, and a failure after the request still is.
    #[test]
    fn a_receipt_reports_the_failures_of_its_own_shot_only() {
        let earlier_failures = crate::streaming::StreamingMetrics {
            failed_cells: 1,
            asset_load_failures: 2,
            material_validation_failures: 1,
            failed_lod_queries: 3,
            failed_lod_chunks: 4,
            ..crate::streaming::StreamingMetrics::default()
        };
        let renderer = crate::render::RendererMetrics::default();
        let baseline = crate::shots::Failures::read(&earlier_failures, &renderer);
        let profiler = crate::profiling::ProfilingState::default();
        let uploads = crate::terrain_upload::TerrainMeshUploadReadiness::default();
        let observed = |work: &mut crate::shots::SettleCounts| {
            work.pending_specializations = Some(0);
            work.pending_render_transfers = Some(0);
        };

        // This shot added nothing, so its receipt is quiet despite the run's failures behind it.
        let ticket = CaptureTicket::new(baseline);
        let mut quiet = captured_work(
            &ticket,
            &earlier_failures,
            &renderer,
            &profiler,
            &uploads,
            true,
        );
        observed(&mut quiet);
        assert!(
            quiet.is_quiet(),
            "earlier failures poison this receipt: {}",
            quiet.describe()
        );

        // A failure between the request and this extraction is this shot's failure.
        let mut during = earlier_failures.clone();
        during.failed_lod_chunks += 1;
        during.failed_cells += 1;
        let mut fresh = captured_work(&ticket, &during, &renderer, &profiler, &uploads, true);
        observed(&mut fresh);
        assert_eq!(
            (fresh.failed_cells, fresh.failed_lod_work),
            (1, 1),
            "a failure after the request is counted"
        );
        assert!(!fresh.is_quiet(), "{}", fresh.describe());

        // Render-side work that has not been observed yet stays unavailable, not quiet.
        let unavailable = captured_work(
            &ticket,
            &earlier_failures,
            &renderer,
            &profiler,
            &uploads,
            true,
        );
        assert!(!unavailable.is_quiet(), "unobserved work is not quiet");

        // The renderer's final path has to be running for the frame to be a settled one.
        let mut not_ready = captured_work(
            &ticket,
            &earlier_failures,
            &renderer,
            &profiler,
            &uploads,
            false,
        );
        observed(&mut not_ready);
        assert!(!not_ready.is_quiet(), "an unready renderer is not settled");

        // Pending LOD work holds the receipt back whatever the failure baseline says.
        let mut pending_lod = earlier_failures.clone();
        pending_lod.pending_lod_queries = 1;
        let mut pending =
            captured_work(&ticket, &pending_lod, &renderer, &profiler, &uploads, true);
        observed(&mut pending);
        assert!(!pending.is_quiet(), "{}", pending.describe());
    }

    #[test]
    fn source_must_match_extracted_camera_before_it_can_label_a_capture() {
        let source = SceneGradeSource::default();
        let mode = SceneGradeMode::SnapshotSrgb;
        let preset = SceneVisualPreset::CoolGray;
        let old_camera = crate::color_pipeline::SceneColorPipeline::default();
        assert!(
            capture_grade(
                Some(&source),
                None,
                Some(&mode),
                Some(&preset),
                Some(&old_camera.grading),
                Some(&old_camera.cinematic_post),
                Some(&old_camera.tonemapping),
                Some(&old_camera.exposure),
                Some(&old_camera.compositing_space),
            )
            .is_none()
        );
    }

    #[test]
    fn tone_metadata_cannot_label_a_camera_with_a_different_mapper() {
        let source = SceneGradeSource::default();
        let mode = SceneGradeMode::SnapshotSrgb;
        let preset = SceneVisualPreset::CoolGray;
        let resolved = source.resolve(mode, preset).unwrap();
        let mut grading = ColorGrading::default();
        grading.global.post_saturation = resolved.bevy_saturation;
        assert!(
            capture_grade(
                Some(&source),
                None,
                Some(&mode),
                Some(&preset),
                Some(&grading),
                Some(&resolved.post),
                Some(&Tonemapping::None),
                Some(&Exposure { ev100: 9.7 }),
                Some(&CompositingSpace::Linear),
            )
            .is_none()
        );
    }
}

// Track asset generations before Bevy drains its extracted lists. Updated generations remove
// their old descriptor before preparation, so a retained descriptor cannot acknowledge old bytes.
#[derive(Resource, Clone, Default)]
pub(crate) struct TransferReadiness(pub Arc<Mutex<Option<usize>>>);

#[derive(Resource, Default)]
struct PendingRenderTransfers {
    meshes: std::collections::HashSet<bevy::asset::AssetId<Mesh>>,
    images: std::collections::HashSet<bevy::asset::AssetId<Image>>,
}

fn observe_render_transfers(
    mut pending: ResMut<PendingRenderTransfers>,
    meshes: Res<ExtractedAssets<RenderMesh>>,
    images: Res<ExtractedAssets<GpuImage>>,
) {
    for id in &meshes.removed {
        pending.meshes.remove(id);
    }
    for id in &images.removed {
        pending.images.remove(id);
    }
    pending
        .meshes
        .extend(meshes.extracted.iter().map(|(id, _)| *id));
    pending
        .images
        .extend(images.extracted.iter().map(|(id, _)| *id));
}

fn publish_render_transfers(
    mut pending: ResMut<PendingRenderTransfers>,
    meshes: Res<RenderAssets<RenderMesh>>,
    images: Res<RenderAssets<GpuImage>>,
    bridge: Res<TransferReadiness>,
) {
    pending.meshes.retain(|id| meshes.get(*id).is_none());
    pending.images.retain(|id| images.get(*id).is_none());
    *bridge.0.lock().unwrap() = Some(pending.meshes.len() + pending.images.len());
}
