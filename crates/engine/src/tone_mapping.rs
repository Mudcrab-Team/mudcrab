//! Explicit retail-curve experiments alongside the approved Bevy display transform.
//!
//! The recovered CPU parameter writer establishes a White-dependent coefficient.
//! It does not establish the effective native branch, GPU upload, sampled
//! adaptation texture, input domain, or final output transfer for a running frame.
//! Experimental parameters therefore require an explicit file and retain their
//! provenance. The approved TonyMcMapface path remains the default.

use crate::{
    color_pipeline::{
        ResolvedSceneGrade, SceneGradeMode, SceneGradeOverrides, SceneGradeSource,
        SceneVisualPreset,
    },
    config::EngineConfig,
};
use bevy::{core_pipeline::tonemapping::Tonemapping, prelude::Resource};
use color_eyre::{Result, eyre::WrapErr};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SceneToneMapMode {
    #[default]
    Tony,
    RetailCurve,
}

impl SceneToneMapMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "tony" => Some(Self::Tony),
            "retail-curve" => Some(Self::RetailCurve),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Tony => "tony",
            Self::RetailCurve => "retail-curve",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RetailCurveKind {
    Reinhard,
    Filmic,
}

impl RetailCurveKind {
    fn operator(self) -> u32 {
        match self {
            Self::Reinhard => 1,
            Self::Filmic => 2,
        }
    }

    fn coefficient_formula(self) -> &'static str {
        match self {
            Self::Reinhard => "1 / (reinhard_white_scale * IMGS.HNAM.white)^2",
            Self::Filmic => {
                "x = max(filmic_white_scale * IMGS.HNAM.white - 0.004, 0); h = x*(6.2*x+0.5)/(x*(6.2*x+1.7)+0.06); coefficient = h^(-2.2)"
            }
        }
    }
}

/// An explicit experiment, not defaults inferred from an authored image space.
///
/// Native PE initializers provide reference white scales of 1.1 and 1.4 and an
/// initially false filmic selector. They do not establish live values. Both
/// scales and the curve selection must be stated in this file. The luminance
/// values stand in for native texture sample P.x/P.y without claiming to
/// reproduce adaptation history. They do not change the approved grade pivot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetailCurveParameters {
    pub name: String,
    pub source: String,
    pub curve: RetailCurveKind,
    pub reinhard_white_scale: f32,
    pub filmic_white_scale: f32,
    pub sampled_luminance: f32,
    pub target_luminance: f32,
}

impl RetailCurveParameters {
    pub fn parse(json: &str) -> Result<Self> {
        let parameters: Self = serde_json::from_str(json)?;
        parameters
            .validate()
            .map_err(color_eyre::eyre::Error::msg)?;
        Ok(parameters)
    }

    fn validate(&self) -> std::result::Result<(), String> {
        if self.name.trim().is_empty() || self.source.trim().is_empty() {
            return Err("tone-map parameters need a nonempty name and source".into());
        }
        for (name, value) in [
            ("reinhard_white_scale", self.reinhard_white_scale),
            ("filmic_white_scale", self.filmic_white_scale),
            ("sampled_luminance", self.sampled_luminance),
            ("target_luminance", self.target_luminance),
        ] {
            if !value.is_finite() || value <= 0.0 {
                return Err(format!("tone-map {name} must be finite and positive"));
            }
        }
        let ratio = self.target_luminance / self.sampled_luminance;
        if !ratio.is_finite() || ratio <= 0.0 {
            return Err(
                "tone-map target/sample luminance ratio must be finite and positive".into(),
            );
        }
        Ok(())
    }

    /// Recover the selected CPU coefficient from an authored White value.
    ///
    /// This follows the recovered conceptual binary32 arithmetic, rather than
    /// promising bitwise equivalence to native instructions or GPU packing.
    pub fn coefficient(&self, white: f32) -> std::result::Result<f32, String> {
        self.validate()?;
        if !white.is_finite() || white <= 0.0 {
            return Err("retail-curve requires a finite positive IMGS.HNAM.White".into());
        }
        let coefficient = match self.curve {
            RetailCurveKind::Reinhard => {
                let scaled_white = self.reinhard_white_scale * white;
                1.0 / (scaled_white * scaled_white)
            }
            RetailCurveKind::Filmic => {
                let x = (self.filmic_white_scale * white - 0.004).max(0.0);
                if x <= 0.0 {
                    return Err(
                        "retail-curve filmic scaled White must exceed the 0.004 black offset"
                            .into(),
                    );
                }
                let h = (x * (6.2 * x + 0.5)) / (x * (6.2 * x + 1.7) + 0.06);
                h.powf(-2.2)
            }
        };
        if !coefficient.is_finite() || coefficient <= 0.0 {
            return Err("retail-curve White/scales produce an unrepresentable coefficient".into());
        }
        Ok(coefficient)
    }
}

#[derive(Resource, Clone, Debug, Default)]
pub struct SceneToneMapSource {
    pub mode: SceneToneMapMode,
    /// Bevy's menu-selected transform for the approved Tony path. The explicit retail curve
    /// continues to resolve to `None` regardless of this preference.
    pub bevy_tonemapping: Tonemapping,
    pub parameters: Option<RetailCurveParameters>,
    pub parameters_path: Option<PathBuf>,
}

impl SceneToneMapSource {
    pub fn load(config: &EngineConfig, grade_source: &SceneGradeSource) -> Result<Self> {
        let parameters = config
            .tone_map_params
            .as_ref()
            .map(|path| {
                let json = std::fs::read_to_string(path).wrap_err_with(|| {
                    format!("cannot read tone-map parameters {}", path.display())
                })?;
                RetailCurveParameters::parse(&json)
                    .wrap_err_with(|| format!("invalid tone-map parameters {}", path.display()))
            })
            .transpose()?;
        let source = Self {
            mode: config.tone_map,
            bevy_tonemapping: Tonemapping::TonyMcMapface,
            parameters,
            parameters_path: config.tone_map_params.clone(),
        };
        source
            .resolve(grade_source, config.color_grade, config.visual_preset)
            .map_err(color_eyre::eyre::Error::msg)?;
        Ok(source)
    }

    pub fn resolve(
        &self,
        grade_source: &SceneGradeSource,
        mode: SceneGradeMode,
        preset: SceneVisualPreset,
    ) -> std::result::Result<ResolvedSceneGrade, String> {
        self.resolve_with_overrides(grade_source, mode, preset, SceneGradeOverrides::default())
    }

    pub fn resolve_with_overrides(
        &self,
        grade_source: &SceneGradeSource,
        mode: SceneGradeMode,
        preset: SceneVisualPreset,
        overrides: SceneGradeOverrides,
    ) -> std::result::Result<ResolvedSceneGrade, String> {
        overrides.validate()?;
        if self.mode == SceneToneMapMode::Tony {
            let mut resolved = grade_source.resolve_with_overrides(mode, preset, overrides)?;
            resolved.tonemapping = self.bevy_tonemapping;
            return Ok(resolved);
        }
        if !matches!(
            mode,
            SceneGradeMode::SnapshotSrgb | SceneGradeMode::RecordSrgb | SceneGradeMode::Off
        ) {
            return Err(
                "retail-curve supports only snapshot-srgb, record-srgb or off grading; legacy saturation/full and snapshot-linear are not mapped to this experiment".into(),
            );
        }
        let parameters = self
            .parameters
            .as_ref()
            .ok_or("retail-curve requires an explicit --tone-map-params <file>")?;
        let hdr = grade_source
            .record
            .as_ref()
            .ok_or("retail-curve requires an explicitly selected image-space record")?
            .hdr
            .as_ref()
            .ok_or("retail-curve requires IMGS.HNAM.White; the selected record has no HNAM")?;
        let coefficient = parameters.coefficient(hdr.white)?;
        let mut resolved = grade_source.resolve_with_overrides(mode, preset, overrides)?;
        resolved.tonemapping = Tonemapping::None;
        resolved.bevy_saturation = 1.0;
        resolved.post.tone_operator = parameters.curve.operator();
        resolved.post.tone_coefficient = coefficient;
        resolved.post.sampled_luminance = parameters.sampled_luminance;
        resolved.post.target_luminance = parameters.target_luminance;
        Ok(resolved)
    }

    pub fn evidence(
        &self,
        grade_source: &SceneGradeSource,
        mode: SceneGradeMode,
        preset: SceneVisualPreset,
    ) -> serde_json::Value {
        self.evidence_with_overrides(grade_source, mode, preset, SceneGradeOverrides::default())
    }

    pub fn evidence_with_overrides(
        &self,
        grade_source: &SceneGradeSource,
        mode: SceneGradeMode,
        preset: SceneVisualPreset,
        overrides: SceneGradeOverrides,
    ) -> serde_json::Value {
        let mut evidence = grade_source.evidence_with_overrides(mode, preset, overrides);
        let resolved = self.resolve_with_overrides(grade_source, mode, preset, overrides);
        let active = self.mode == SceneToneMapMode::RetailCurve && resolved.is_ok();
        let white = grade_source
            .record
            .as_ref()
            .and_then(|record| record.hdr.as_ref())
            .map(|hdr| hdr.white);
        let mut unapplied_fields = vec![
            "eye_adaptation_speed",
            "bloom_blur_radius",
            "bloom_threshold",
            "bloom_scale",
            "receive_bloom_threshold",
            "sunlight_scale",
            "sky_scale",
            "eye_adaptation_strength",
        ];
        if !active {
            unapplied_fields.push("white");
        }
        evidence["tone_map_mode"] = self.mode.name().into();
        evidence["tonemapping"] = match self.mode {
            SceneToneMapMode::Tony => resolved
                .as_ref()
                .ok()
                .map(|resolved| format!("{:?}", resolved.tonemapping))
                .unwrap_or_else(|| "unresolved".into()),
            SceneToneMapMode::RetailCurve => "RetailCurveExperiment".into(),
        }
        .into();
        evidence["bevy_tonemapping"] = resolved
            .as_ref()
            .ok()
            .map(|resolved| format!("{:?}", resolved.tonemapping))
            .into();
        evidence["hnam_applied"] = active.into();
        evidence["hnam_fields_applied"] =
            serde_json::json!(if active { vec!["white"] } else { vec![] });
        evidence["hnam_fields_unapplied"] = serde_json::json!(unapplied_fields);
        evidence["effective_grade"] = resolved
            .as_ref()
            .ok()
            .map(|resolved| serde_json::json!(resolved.grade))
            .into();
        evidence["effective_stages"] = resolved
            .as_ref()
            .ok()
            .map(|resolved| {
                serde_json::json!({
                    "bevy_post_saturation": resolved.bevy_saturation,
                    "post_enabled": resolved.post.enabled,
                    "post_saturation": resolved.post.saturation,
                    "post_linear_domain": resolved.post.linear_domain,
                    "pivot": resolved.post.pivot,
                    "tone_operator": resolved.post.tone_operator,
                    "tone_coefficient": resolved.post.tone_coefficient,
                    "sampled_luminance": resolved.post.sampled_luminance,
                    "target_luminance": resolved.post.target_luminance,
                })
            })
            .into();
        evidence["pivot"] = resolved
            .as_ref()
            .ok()
            .map(|resolved| resolved.post.pivot)
            .into();
        let grade_pivot_source = if resolved.is_err() {
            "unresolved"
        } else if overrides.pivot.is_some() {
            "explicit user pivot override; independent of sampled_luminance"
        } else {
            "approved artistic pivot; independent of sampled_luminance"
        };
        evidence["tone_mapping"] = serde_json::json!({
            "mode": self.mode,
            "applied": active,
            "parameters_path": self.parameters_path,
            "parameters": self.parameters,
            "loaded_white": white,
            "coefficient": if active { resolved.as_ref().ok().map(|resolved| resolved.post.tone_coefficient) } else { None },
            "coefficient_formula": self.parameters.as_ref().map(|parameters| parameters.curve.coefficient_formula()),
            "coefficient_cpu_producer": "HDRParameters0x14153D860",
            "coefficient_input": "selected IMGS.HNAM.White and explicit parameter-file white scales",
            "native_gpu_upload_verified": false,
            "native_runtime_parameters_observed": false,
            "luminance_source": "explicit experimental overrides for native texture sample P.x and P.y; no adaptation history",
            "grade_pivot_source": grade_pivot_source,
            "native_final_exponent_applied": false,
            "bloom_input_applied": false,
        });
        evidence["resolution_error"] = resolved.err().into();
        if self.mode == SceneToneMapMode::RetailCurve {
            let grade_description = match mode {
                SceneGradeMode::SnapshotSrgb => "approved sRGB grade",
                SceneGradeMode::RecordSrgb => "authored sRGB grade",
                SceneGradeMode::Off => "grading off",
                _ => "unsupported grading mode; experiment did not resolve",
            };
            evidence["limits"] = format!("retail-curve experiment; Bevy exposed linear input; {grade_description}; only HNAM.White contributes to the CPU-derived coefficient; native sampled domain, P history, bloom, final native exponent, native GPU upload and modifier composition remain unresolved; authored slot is not effective runtime weather").into();
        }
        evidence
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::imagespace::{
        ImageSpaceCinematic, ImageSpaceHdr, ImageSpaceSelection, ImageSpaceTint, LoadedImageSpace,
    };

    fn parameters(curve: RetailCurveKind) -> RetailCurveParameters {
        RetailCurveParameters {
            name: "explicit test experiment".into(),
            source: "static white scales; explicit unmeasured test luminance values".into(),
            curve,
            reinhard_white_scale: 1.1,
            filmic_white_scale: 1.4,
            sampled_luminance: 0.2,
            target_luminance: 0.3,
        }
    }

    fn grade_source(white: Option<f32>) -> SceneGradeSource {
        let mut source = SceneGradeSource::default();
        source.record = Some(LoadedImageSpace {
            selection: ImageSpaceSelection::Direct(0x12F87),
            source_weather: None,
            form_id: 0x12F87,
            editor_id: Some("test-authored-image-space".into()),
            load_order: 1,
            winner_plugin: Some("test-winner.esm".into()),
            hdr: white.map(|white| ImageSpaceHdr {
                eye_adaptation_speed: 40.0,
                bloom_blur_radius: 7.0,
                bloom_threshold: 0.8,
                bloom_scale: 3.0,
                receive_bloom_threshold: 0.7,
                white,
                sunlight_scale: 3.0,
                sky_scale: 0.16,
                eye_adaptation_strength: 5.0,
            }),
            cinematic: ImageSpaceCinematic {
                saturation: 1.6,
                brightness: 1.15,
                contrast: 1.5,
            },
            tint: ImageSpaceTint {
                amount: 0.5625,
                rgb: [0.882_352_95, 0.807_843_15, 0.717_647_1],
            },
        });
        source
    }

    fn experiment(curve: RetailCurveKind) -> SceneToneMapSource {
        SceneToneMapSource {
            mode: SceneToneMapMode::RetailCurve,
            bevy_tonemapping: Tonemapping::TonyMcMapface,
            parameters: Some(parameters(curve)),
            parameters_path: Some("explicit-test-parameters.json".into()),
        }
    }

    #[test]
    fn default_and_mode_names_preserve_tony() {
        assert_eq!(SceneToneMapSource::default().mode, SceneToneMapMode::Tony);
        assert!(SceneToneMapSource::default().parameters.is_none());
        for mode in [SceneToneMapMode::Tony, SceneToneMapMode::RetailCurve] {
            assert_eq!(SceneToneMapMode::parse(mode.name()), Some(mode));
        }
        assert_eq!(
            SceneToneMapMode::parse("RETAIL-CURVE"),
            Some(SceneToneMapMode::RetailCurve)
        );
        assert_eq!(SceneToneMapMode::parse("native"), None);
    }

    #[test]
    fn recovered_coefficients_match_independent_numeric_references() {
        for (white, reinhard, filmic) in [
            (1.0, 0.826_446_3, 1.323_234_7),
            (1.05, 0.749_611_14, 1.306_991_1),
        ] {
            assert!(
                (parameters(RetailCurveKind::Reinhard)
                    .coefficient(white)
                    .unwrap()
                    - reinhard)
                    .abs()
                    < 0.000_002
            );
            assert!(
                (parameters(RetailCurveKind::Filmic)
                    .coefficient(white)
                    .unwrap()
                    - filmic)
                    .abs()
                    < 0.000_002
            );
        }
    }

    #[test]
    fn parameters_reject_missing_unknown_and_invalid_values() {
        let valid = serde_json::to_value(parameters(RetailCurveKind::Reinhard)).unwrap();
        RetailCurveParameters::parse(&valid.to_string()).unwrap();
        for field in [
            "name",
            "source",
            "curve",
            "reinhard_white_scale",
            "filmic_white_scale",
            "sampled_luminance",
            "target_luminance",
        ] {
            let mut missing = valid.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(
                RetailCurveParameters::parse(&missing.to_string()).is_err(),
                "missing {field}"
            );
        }
        let mut unknown = valid.clone();
        unknown["white"] = 1.0.into();
        assert!(RetailCurveParameters::parse(&unknown.to_string()).is_err());
        for field in [
            "reinhard_white_scale",
            "filmic_white_scale",
            "sampled_luminance",
            "target_luminance",
        ] {
            for value in [0.0, -1.0] {
                let mut invalid = valid.clone();
                invalid[field] = value.into();
                assert!(RetailCurveParameters::parse(&invalid.to_string()).is_err());
            }
        }
        for field in ["name", "source"] {
            let mut invalid = valid.clone();
            invalid[field] = "  ".into();
            assert!(RetailCurveParameters::parse(&invalid.to_string()).is_err());
        }
        let mut invalid = parameters(RetailCurveKind::Filmic);
        invalid.sampled_luminance = f32::NAN;
        assert!(invalid.coefficient(1.0).is_err());
        invalid.sampled_luminance = f32::MIN_POSITIVE;
        invalid.target_luminance = f32::MAX;
        assert!(invalid.coefficient(1.0).is_err());
    }

    #[test]
    fn experiment_requires_parameters_record_and_positive_white() {
        let tone = experiment(RetailCurveKind::Reinhard);
        let mode = SceneGradeMode::SnapshotSrgb;
        let preset = SceneVisualPreset::CoolGray;
        assert!(
            tone.resolve(&SceneGradeSource::default(), mode, preset)
                .is_err()
        );
        assert!(tone.resolve(&grade_source(None), mode, preset).is_err());
        for white in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(
                tone.resolve(&grade_source(Some(white)), mode, preset)
                    .is_err()
            );
        }
        let mut missing = tone;
        missing.parameters = None;
        assert!(
            missing
                .resolve(&grade_source(Some(1.0)), mode, preset)
                .is_err()
        );
        assert!(
            parameters(RetailCurveKind::Filmic)
                .coefficient(0.001)
                .is_err()
        );
        assert!(
            parameters(RetailCurveKind::Reinhard)
                .coefficient(f32::MAX)
                .is_err()
        );
    }

    #[test]
    fn grade_overrides_preserve_retail_curve_white_and_luminance_parameters() {
        let tone = experiment(RetailCurveKind::Filmic);
        let source = grade_source(Some(1.05));
        let overrides = SceneGradeOverrides {
            brightness: Some(1.8),
            pivot: Some(0.29),
            ..Default::default()
        };
        let resolved = tone
            .resolve_with_overrides(
                &source,
                SceneGradeMode::RecordSrgb,
                SceneVisualPreset::CoolGray,
                overrides,
            )
            .unwrap();
        assert_eq!(resolved.grade.brightness, 1.8);
        assert_eq!(resolved.post.pivot, 0.29);
        assert_eq!(resolved.post.tone_operator, 2);
        assert_eq!(resolved.post.sampled_luminance, 0.2);
        assert_eq!(resolved.post.target_luminance, 0.3);
        assert!(
            (resolved.post.tone_coefficient
                - parameters(RetailCurveKind::Filmic)
                    .coefficient(1.05)
                    .unwrap())
            .abs()
                < f32::EPSILON
        );
        let evidence = tone.evidence_with_overrides(
            &source,
            SceneGradeMode::RecordSrgb,
            SceneVisualPreset::CoolGray,
            overrides,
        );
        assert_eq!(
            evidence["loaded_record"]["hdr"]["white"],
            serde_json::json!(1.05_f32)
        );
        assert_eq!(
            evidence["tone_mapping"]["loaded_white"],
            serde_json::json!(1.05_f32)
        );
        assert_eq!(
            evidence["grade_overrides"]["brightness"],
            serde_json::json!(1.8_f32)
        );
        assert_eq!(evidence["effective_stages"]["tone_operator"], 2);
        assert_eq!(evidence["pivot"], serde_json::json!(0.29_f32));
        assert_eq!(evidence["effective_stages"]["pivot"], evidence["pivot"]);
        assert_eq!(evidence["default_pivot"], serde_json::json!(0.18_f32));
        assert_eq!(
            evidence["tone_mapping"]["grade_pivot_source"],
            "explicit user pivot override; independent of sampled_luminance"
        );
        let untuned = tone.evidence(
            &source,
            SceneGradeMode::RecordSrgb,
            SceneVisualPreset::CoolGray,
        );
        assert_eq!(untuned["pivot"], serde_json::json!(0.18_f32));
        assert_eq!(
            untuned["tone_mapping"]["grade_pivot_source"],
            "approved artistic pivot; independent of sampled_luminance"
        );
        let mut missing_parameters = tone;
        missing_parameters.parameters = None;
        let unresolved = missing_parameters.evidence_with_overrides(
            &source,
            SceneGradeMode::RecordSrgb,
            SceneVisualPreset::CoolGray,
            overrides,
        );
        assert!(unresolved["pivot"].is_null());
        assert!(unresolved["effective_stages"].is_null());
        assert!(unresolved["resolution_error"].is_string());
        assert_eq!(
            unresolved["tone_mapping"]["grade_pivot_source"],
            "unresolved"
        );
    }

    #[test]
    fn unsupported_grades_are_rejected_before_bypassing_bevy_saturation() {
        let tone = experiment(RetailCurveKind::Reinhard);
        let source = grade_source(Some(1.05));
        for mode in [
            SceneGradeMode::Full,
            SceneGradeMode::Saturation,
            SceneGradeMode::SnapshotLinear,
        ] {
            assert!(
                tone.resolve(&source, mode, SceneVisualPreset::CoolGray)
                    .is_err()
            );
        }
    }

    #[test]
    fn curve_preserves_approved_and_record_grade_and_leaves_off_ungraded() {
        let tone = experiment(RetailCurveKind::Reinhard);
        let source = grade_source(Some(1.05));
        for mode in [
            SceneGradeMode::SnapshotSrgb,
            SceneGradeMode::RecordSrgb,
            SceneGradeMode::Off,
        ] {
            let before = source.resolve(mode, SceneVisualPreset::CoolGray).unwrap();
            let after = tone
                .resolve(&source, mode, SceneVisualPreset::CoolGray)
                .unwrap();
            assert_eq!(after.grade, before.grade);
            let mut expected_post = before.post;
            expected_post.tone_operator = 1;
            expected_post.tone_coefficient = parameters(RetailCurveKind::Reinhard)
                .coefficient(1.05)
                .unwrap();
            expected_post.sampled_luminance = 0.2;
            expected_post.target_luminance = 0.3;
            assert_eq!(after.post, expected_post);
            assert_eq!(after.bevy_saturation, 1.0);
            assert_eq!(after.tonemapping, Tonemapping::None);
            assert_eq!(
                after.post.enabled,
                if mode == SceneGradeMode::Off {
                    0.0
                } else {
                    1.0
                }
            );
            let evidence = tone.evidence(&source, mode, SceneVisualPreset::CoolGray);
            let limits = evidence["limits"].as_str().unwrap();
            let expected_description = match mode {
                SceneGradeMode::SnapshotSrgb => "approved sRGB grade",
                SceneGradeMode::RecordSrgb => "authored sRGB grade",
                SceneGradeMode::Off => "grading off",
                _ => unreachable!(),
            };
            assert!(limits.contains(expected_description));
            if mode != SceneGradeMode::SnapshotSrgb {
                assert!(!limits.contains("approved sRGB grade"));
            }
        }
        let evidence = tone.evidence(
            &source,
            SceneGradeMode::RecordSrgb,
            SceneVisualPreset::CoolGray,
        );
        assert_eq!(evidence["hnam_applied"], true);
        assert_eq!(
            evidence["hnam_fields_applied"],
            serde_json::json!(["white"])
        );
        assert_eq!(
            evidence["hnam_fields_unapplied"].as_array().unwrap().len(),
            8
        );
        assert_eq!(
            evidence["loaded_record"]["winner_plugin"],
            "test-winner.esm"
        );
        assert_eq!(
            evidence["tone_mapping"]["loaded_white"],
            serde_json::json!(1.05_f32)
        );
        assert_eq!(
            evidence["tone_mapping"]["native_gpu_upload_verified"],
            false
        );
        assert_eq!(evidence["bevy_tonemapping"], "None");
    }

    #[test]
    fn tony_can_preload_parameters_without_a_record_or_changing_the_look() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("explicit.json");
        std::fs::write(
            &path,
            serde_json::to_string(&parameters(RetailCurveKind::Filmic)).unwrap(),
        )
        .unwrap();
        let config = EngineConfig {
            tone_map: SceneToneMapMode::Tony,
            tone_map_params: Some(path.clone()),
            ..Default::default()
        };
        let source = SceneGradeSource::default();
        let tone = SceneToneMapSource::load(&config, &source).unwrap();
        assert_eq!(tone.parameters_path.as_ref(), Some(&path));
        assert!(tone.parameters.is_some());
        let before = source
            .resolve(config.color_grade, config.visual_preset)
            .unwrap();
        let after = tone
            .resolve(&source, config.color_grade, config.visual_preset)
            .unwrap();
        assert_eq!(after.grade, before.grade);
        assert_eq!(after.post, before.post);
        assert_eq!(after.bevy_saturation, before.bevy_saturation);
        assert_eq!(after.tonemapping, Tonemapping::TonyMcMapface);
        let evidence = tone.evidence(&source, config.color_grade, config.visual_preset);
        assert_eq!(evidence["tonemapping"], "TonyMcMapface");
        assert_eq!(evidence["hnam_applied"], false);
        assert_eq!(
            evidence["hnam_fields_unapplied"].as_array().unwrap().len(),
            9
        );
        assert!(evidence["tone_mapping"]["coefficient"].is_null());

        std::fs::write(&path, "{}").unwrap();
        assert!(SceneToneMapSource::load(&config, &source).is_err());
    }

    #[test]
    fn approved_tonemap_selection_flows_through_shared_grade_resolution() {
        let source = SceneGradeSource::default();
        let tone = SceneToneMapSource {
            bevy_tonemapping: Tonemapping::AgX,
            ..Default::default()
        };
        let resolved = tone
            .resolve(
                &source,
                SceneGradeMode::SnapshotSrgb,
                SceneVisualPreset::CoolGray,
            )
            .unwrap();
        assert_eq!(resolved.tonemapping, Tonemapping::AgX);
        assert_eq!(
            tone.evidence(
                &source,
                SceneGradeMode::SnapshotSrgb,
                SceneVisualPreset::CoolGray
            )["tonemapping"],
            "AgX"
        );
        let retail = experiment(RetailCurveKind::Reinhard);
        assert_eq!(
            retail
                .resolve(
                    &grade_source(Some(1.05)),
                    SceneGradeMode::SnapshotSrgb,
                    SceneVisualPreset::CoolGray,
                )
                .unwrap()
                .tonemapping,
            Tonemapping::None,
            "the explicit curve owns camera tonemapping"
        );
    }
}
