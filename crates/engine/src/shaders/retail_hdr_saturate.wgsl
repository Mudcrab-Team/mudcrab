// The saturate that ends retail's fog composite (NEEDS 18 D3: records 49, 51 and 53 end in
// `mad_sat`). `retail_hdr.rs` draws this with the Min blend operation, so the target becomes
// min(1, target) per channel. The fog pass is a blend and cannot clip, and the HDR target holds
// values above 1.
#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(1.0);
}
