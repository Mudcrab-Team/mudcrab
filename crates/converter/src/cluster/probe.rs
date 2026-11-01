//! Capability probe for `--lod-mode auto` (ADR-0013 section 6): asks the converting machine's GPU
//! adapter whether the engine's cluster draw path can run on it, before any LOD work starts.
//!
//! The probe only reads adapter info, features and limits. It never opens a device.

use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use serde_json::{Value, json};

/// Storage buffers one shader stage must be able to bind. Placeholder until the engine prototype
/// declares its own number; it matches wgpu's default limit today.
pub const REQUIRED_STORAGE_BUFFERS_PER_STAGE: u32 = 8;
/// Largest single storage buffer binding the cluster path needs, in bytes.
pub const REQUIRED_MAX_STORAGE_BUFFER_BINDING_SIZE: u32 = 128 * 1024 * 1024;
/// Bind groups a pipeline layout must be able to hold.
pub const REQUIRED_MAX_BIND_GROUPS: u32 = 4;

/// Outcome of the probe. `json` is stored verbatim in `cluster_build.probe_json`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProbeResult {
    pub passed: bool,
    /// Human-readable list of required items the adapter lacks; empty when passed.
    pub missing: Vec<String>,
    /// "Vulkan", "Dx12", "Metal", ..., or "none" when there is no adapter.
    pub backend: String,
    /// Empty when there is no adapter.
    pub adapter_name: String,
    /// What was probed: backend, adapter name, device type, driver, and the required features and
    /// limits next to the values the adapter reports.
    pub json: Value,
}

/// Probes the machine's best adapter. A machine without any adapter fails with `"no GPU adapter"`.
pub fn probe_capabilities() -> ProbeResult {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }));
    let adapter = match adapter {
        Ok(adapter) => adapter,
        Err(error) => return no_adapter(&error.to_string()),
    };
    let info = adapter.get_info();
    let mut result = evaluate(
        info.backend,
        &info.name,
        adapter.features(),
        &adapter.limits(),
        info.device_type,
    );
    if let Value::Object(map) = &mut result.json {
        map.insert("driver".into(), json!(info.driver));
        map.insert("driver_info".into(), json!(info.driver_info));
    }
    result
}

/// The probe's rules applied to values already read from an adapter.
pub fn evaluate(
    backend: wgpu::Backend,
    adapter_name: &str,
    features: wgpu::Features,
    limits: &wgpu::Limits,
    device_type: wgpu::DeviceType,
) -> ProbeResult {
    let mut missing = Vec::new();
    let backend_supported = matches!(
        backend,
        wgpu::Backend::Vulkan | wgpu::Backend::Dx12 | wgpu::Backend::Metal
    );
    if !backend_supported {
        missing.push(format!("backend {backend:?} (need Vulkan, Dx12 or Metal)"));
    }
    if device_type == wgpu::DeviceType::Cpu {
        missing.push("software adapter".to_string());
    }
    let has_indirect_first_instance = features.contains(wgpu::Features::INDIRECT_FIRST_INSTANCE);
    if !has_indirect_first_instance {
        missing.push("feature INDIRECT_FIRST_INSTANCE".to_string());
    }
    if limits.max_storage_buffers_per_shader_stage < REQUIRED_STORAGE_BUFFERS_PER_STAGE {
        missing.push(format!(
            "limit max_storage_buffers_per_shader_stage {} (need {})",
            limits.max_storage_buffers_per_shader_stage, REQUIRED_STORAGE_BUFFERS_PER_STAGE
        ));
    }
    if limits.max_storage_buffer_binding_size < u64::from(REQUIRED_MAX_STORAGE_BUFFER_BINDING_SIZE)
    {
        missing.push(format!(
            "limit max_storage_buffer_binding_size {} (need {})",
            limits.max_storage_buffer_binding_size, REQUIRED_MAX_STORAGE_BUFFER_BINDING_SIZE
        ));
    }
    if limits.max_bind_groups < REQUIRED_MAX_BIND_GROUPS {
        missing.push(format!(
            "limit max_bind_groups {} (need {})",
            limits.max_bind_groups, REQUIRED_MAX_BIND_GROUPS
        ));
    }
    let backend_name = format!("{backend:?}");
    let json = json!({
        "backend": backend_name,
        "adapter_name": adapter_name,
        "device_type": format!("{device_type:?}"),
        "required_backends": ["Vulkan", "Dx12", "Metal"],
        "features": {
            "INDIRECT_FIRST_INSTANCE": {
                "required": true,
                "reported": has_indirect_first_instance,
            },
        },
        "limits": {
            "max_storage_buffers_per_shader_stage": {
                "required": REQUIRED_STORAGE_BUFFERS_PER_STAGE,
                "reported": limits.max_storage_buffers_per_shader_stage,
            },
            "max_storage_buffer_binding_size": {
                "required": REQUIRED_MAX_STORAGE_BUFFER_BINDING_SIZE,
                "reported": limits.max_storage_buffer_binding_size,
            },
            "max_bind_groups": {
                "required": REQUIRED_MAX_BIND_GROUPS,
                "reported": limits.max_bind_groups,
            },
        },
        "missing": missing,
    });
    ProbeResult {
        passed: missing.is_empty(),
        missing,
        backend: backend_name,
        adapter_name: adapter_name.to_string(),
        json,
    }
}

fn no_adapter(reason: &str) -> ProbeResult {
    let missing = vec!["no GPU adapter".to_string()];
    let json = json!({
        "backend": "none",
        "adapter_name": "",
        "error": reason,
        "missing": missing,
    });
    ProbeResult {
        passed: false,
        missing,
        backend: "none".to_string(),
        adapter_name: String::new(),
        json,
    }
}

/// wgpu's native futures resolve without an executor; poll until ready.
fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return value;
        }
        std::thread::yield_now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passing_limits() -> wgpu::Limits {
        wgpu::Limits {
            max_storage_buffers_per_shader_stage: REQUIRED_STORAGE_BUFFERS_PER_STAGE,
            max_storage_buffer_binding_size: u64::from(REQUIRED_MAX_STORAGE_BUFFER_BINDING_SIZE),
            max_bind_groups: REQUIRED_MAX_BIND_GROUPS,
            ..wgpu::Limits::downlevel_defaults()
        }
    }

    fn run(
        backend: wgpu::Backend,
        features: wgpu::Features,
        limits: &wgpu::Limits,
        device_type: wgpu::DeviceType,
    ) -> ProbeResult {
        evaluate(backend, "Test GPU", features, limits, device_type)
    }

    #[test]
    fn passing_values_pass_on_every_supported_backend() {
        for backend in [
            wgpu::Backend::Vulkan,
            wgpu::Backend::Dx12,
            wgpu::Backend::Metal,
        ] {
            let result = run(
                backend,
                wgpu::Features::INDIRECT_FIRST_INSTANCE,
                &passing_limits(),
                wgpu::DeviceType::DiscreteGpu,
            );
            assert!(result.passed, "{backend:?}: {:?}", result.missing);
            assert!(result.missing.is_empty());
            assert_eq!(result.backend, format!("{backend:?}"));
            assert_eq!(result.adapter_name, "Test GPU");
        }
    }

    #[test]
    fn json_names_backend_adapter_and_values() {
        let result = run(
            wgpu::Backend::Dx12,
            wgpu::Features::INDIRECT_FIRST_INSTANCE,
            &passing_limits(),
            wgpu::DeviceType::IntegratedGpu,
        );
        assert_eq!(result.json["backend"], "Dx12");
        assert_eq!(result.json["adapter_name"], "Test GPU");
        assert_eq!(result.json["device_type"], "IntegratedGpu");
        assert_eq!(
            result.json["features"]["INDIRECT_FIRST_INSTANCE"]["reported"],
            true
        );
        assert_eq!(
            result.json["limits"]["max_bind_groups"]["reported"],
            REQUIRED_MAX_BIND_GROUPS
        );
        assert_eq!(
            result.json["limits"]["max_storage_buffer_binding_size"]["required"],
            REQUIRED_MAX_STORAGE_BUFFER_BINDING_SIZE
        );
    }

    #[test]
    fn missing_feature_is_named() {
        let result = run(
            wgpu::Backend::Vulkan,
            wgpu::Features::empty(),
            &passing_limits(),
            wgpu::DeviceType::DiscreteGpu,
        );
        assert!(!result.passed);
        assert_eq!(result.missing.len(), 1);
        assert!(result.missing[0].contains("INDIRECT_FIRST_INSTANCE"));
        assert_eq!(
            result.json["features"]["INDIRECT_FIRST_INSTANCE"]["reported"],
            false
        );
    }

    #[test]
    fn each_low_limit_is_named() {
        let cases: [(&str, wgpu::Limits); 3] = [
            (
                "max_storage_buffers_per_shader_stage",
                wgpu::Limits {
                    max_storage_buffers_per_shader_stage: REQUIRED_STORAGE_BUFFERS_PER_STAGE - 1,
                    ..passing_limits()
                },
            ),
            (
                "max_storage_buffer_binding_size",
                wgpu::Limits {
                    max_storage_buffer_binding_size: u64::from(
                        REQUIRED_MAX_STORAGE_BUFFER_BINDING_SIZE,
                    ) - 1,
                    ..passing_limits()
                },
            ),
            (
                "max_bind_groups",
                wgpu::Limits {
                    max_bind_groups: REQUIRED_MAX_BIND_GROUPS - 1,
                    ..passing_limits()
                },
            ),
        ];
        for (name, limits) in cases {
            let result = run(
                wgpu::Backend::Vulkan,
                wgpu::Features::INDIRECT_FIRST_INSTANCE,
                &limits,
                wgpu::DeviceType::DiscreteGpu,
            );
            assert!(!result.passed, "{name}");
            assert_eq!(result.missing.len(), 1, "{name}: {:?}", result.missing);
            assert!(result.missing[0].contains(name), "{name}");
        }
    }

    #[test]
    fn gl_backend_is_missing() {
        let result = run(
            wgpu::Backend::Gl,
            wgpu::Features::INDIRECT_FIRST_INSTANCE,
            &passing_limits(),
            wgpu::DeviceType::DiscreteGpu,
        );
        assert!(!result.passed);
        assert_eq!(result.missing.len(), 1);
        assert!(result.missing[0].contains("Gl"));
        assert_eq!(result.backend, "Gl");
    }

    #[test]
    fn software_adapter_is_missing() {
        let result = run(
            wgpu::Backend::Vulkan,
            wgpu::Features::INDIRECT_FIRST_INSTANCE,
            &passing_limits(),
            wgpu::DeviceType::Cpu,
        );
        assert!(!result.passed);
        assert_eq!(result.missing, vec!["software adapter".to_string()]);
    }

    #[test]
    fn several_problems_are_all_listed() {
        let result = run(
            wgpu::Backend::Gl,
            wgpu::Features::empty(),
            &wgpu::Limits {
                max_bind_groups: 1,
                ..passing_limits()
            },
            wgpu::DeviceType::Cpu,
        );
        assert_eq!(result.missing.len(), 4);
        assert_eq!(result.json["missing"].as_array().map(Vec::len), Some(4));
    }

    #[test]
    fn no_adapter_result_is_a_failed_probe() {
        let result = no_adapter("test");
        assert!(!result.passed);
        assert_eq!(result.missing, vec!["no GPU adapter".to_string()]);
        assert_eq!(result.backend, "none");
        assert_eq!(result.adapter_name, "");
        assert_eq!(result.json["backend"], "none");
    }

    #[test]
    fn probing_this_machine_does_not_panic() {
        let result = probe_capabilities();
        assert!(result.json.is_object());
        assert_eq!(result.json["backend"], result.backend.as_str());
        assert_eq!(result.passed, result.missing.is_empty());
    }
}
