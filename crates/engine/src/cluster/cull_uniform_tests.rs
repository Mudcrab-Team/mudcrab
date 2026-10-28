use super::*;
use bytemuck::Zeroable;

#[test]
fn cull_uniform_uploads_once_until_shader_inputs_change() {
    let mut uploaded = None;
    let mut params = CullParams::zeroed();
    let first = cull_params_to_upload(&mut uploaded, params).expect("first upload");
    assert_eq!(first, &[0; CULL_PARAMS_BYTES]);
    assert!(cull_params_to_upload(&mut uploaded, params).is_none());
    params.epoch = 42;
    assert!(cull_params_to_upload(&mut uploaded, params).is_none());
    params.instance_count = 17;
    let bytes = cull_params_to_upload(&mut uploaded, params).expect("new instances");
    assert_eq!(&bytes[108..112], &17_u32.to_ne_bytes());
    assert_eq!(&bytes[124..128], &[0; 4]);
    assert!(cull_params_to_upload(&mut uploaded, params).is_none());
    uploaded = None;
    assert!(cull_params_to_upload(&mut uploaded, params).is_some());
}

#[test]
fn cull_uniform_upload_observes_every_shader_input_byte() {
    let params = CullParams::zeroed();
    for byte in 0..std::mem::offset_of!(CullParams, epoch) {
        let mut uploaded = Some(params);
        let mut changed = params;
        bytemuck::bytes_of_mut(&mut changed)[byte] = 1;
        let bytes = cull_params_to_upload(&mut uploaded, changed)
            .unwrap_or_else(|| panic!("shader input byte {byte} changed"));
        assert_eq!(bytes[byte], 1);
    }
}

#[test]
fn cull_uniform_upload_compares_float_bits_including_nan_and_signed_zero() {
    let mut uploaded = None;
    let mut params = CullParams::zeroed();
    params.near = f32::from_bits(0x7fc0_0001);
    assert!(cull_params_to_upload(&mut uploaded, params).is_some());
    assert!(cull_params_to_upload(&mut uploaded, params).is_none());
    params.near = f32::from_bits(0x7fc0_0002);
    assert!(cull_params_to_upload(&mut uploaded, params).is_some());
    params.near = 0.0;
    assert!(cull_params_to_upload(&mut uploaded, params).is_some());
    params.near = -0.0;
    assert!(cull_params_to_upload(&mut uploaded, params).is_some());
}
