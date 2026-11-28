use crate::material::{MaterialSwapState, MaterialPreparationState, ValidatedNifMaterial, NifShaderFamily, LightingShaderType, NifAlphaMode, NifTextureSlot, NifTextureSemantic};
use std::path::PathBuf;

#[test]
fn test_material_swap_state_initialization() {
    let material = create_test_material();
    let mut state = MaterialSwapState::new(material);
    
    assert_eq!(state.current_material.shader_family, NifShaderFamily::Lighting);
    assert!(state.pending_swap.is_none());
    assert_eq!(state.preparation_state, MaterialPreparationState::Unprepared);
}

#[test]
fn test_material_swap_start() {
    let material = create_test_material();
    let mut state = MaterialSwapState::new(material);
    let new_material = create_test_material();
    
    state.start_swap(new_material.clone());
    
    assert!(state.pending_swap.is_some());
    assert_eq!(state.pending_swap.as_ref().unwrap().shader_family, NifShaderFamily::Lighting);
    assert_eq!(state.preparation_state, MaterialPreparationState::Unprepared);
}

#[test]
fn test_material_swap_mark_image_ready() {
    let material = create_test_material();
    let mut state = MaterialSwapState::new(material);
    let new_material = create_test_material();
    
    state.start_swap(new_material);
    state.mark_image_ready();
    
    assert_eq!(state.preparation_state, MaterialPreparationState::ImageReady);
}

#[test]
fn test_material_swap_mark_render_ready() {
    let material = create_test_material();
    let mut state = MaterialSwapState::new(material);
    let new_material = create_test_material();
    
    state.start_swap(new_material);
    state.mark_image_ready();
    let ready = state.mark_render_ready();
    
    assert!(ready);
    assert_eq!(state.preparation_state, MaterialPreparationState::RenderReady);
}

#[test]
fn test_material_swap_complete() {
    let material = create_test_material();
    let mut state = MaterialSwapState::new(material);
    let new_material = create_test_material();
    
    state.start_swap(new_material.clone());
    state.mark_image_ready();
    state.mark_render_ready();
    let completed = state.complete_swap_if_ready();
    
    assert!(completed);
    assert!(state.pending_swap.is_none());
    assert_eq!(state.preparation_state, MaterialPreparationState::Unprepared);
    assert_eq!(state.current_material.shader_family, NifShaderFamily::Lighting);
}

#[test]
fn test_material_swap_early_complete_fails() {
    let material = create_test_material();
    let mut state = MaterialSwapState::new(material);
    let new_material = create_test_material();
    
    state.start_swap(new_material);
    let completed = state.complete_swap_if_ready();
    
    assert!(!completed);
    assert!(state.pending_swap.is_some());
}

#[test]
fn test_material_swap_image_ready_complete_fails() {
    let material = create_test_material();
    let mut state = MaterialSwapState::new(material);
    let new_material = create_test_material();
    
    state.start_swap(new_material);
    state.mark_image_ready();
    let completed = state.complete_swap_if_ready();
    
    assert!(!completed);
    assert!(state.pending_swap.is_some());
}

fn create_test_material() -> ValidatedNifMaterial {
    ValidatedNifMaterial {
        shader_family: NifShaderFamily::Lighting,
        lighting_shader_type: Some(LightingShaderType::Default),
        shader_block: 0,
        texture_set_block: None,
        alpha_property_block: None,
        alpha_property: None,
        shader_flags_1: 0,
        shader_flags_2: 0,
        base_color: [1.0, 1.0, 1.0, 1.0],
        alpha: 1.0,
        alpha_mode: NifAlphaMode::Opaque,
        alpha_threshold: None,
        glossiness: 0.5,
        specular_color: [1.0, 1.0, 1.0],
        specular_strength: 0.5,
        emissive_color: [0.0, 0.0, 0.0],
        emissive_multiple: 0.0,
        double_sided: false,
        textures: vec![
            NifTextureSlot {
                slot: 0,
                semantic: NifTextureSemantic::Diffuse,
                path: "test.dds".to_string(),
                required: true,
            }
        ],
        uv_offset: [0.0, 0.0],
        uv_scale: [1.0, 1.0],
        texture_clamp_mode: 0,
        tint_color: None,
    }
}