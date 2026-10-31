use super::update_joint_matrix;
use bevy_math::Mat4;

#[test]
fn signed_zero_upload_changes_invalidate_in_the_same_call() {
    let initial = Mat4::IDENTITY.to_cols_array();
    for word in (0..16).filter(|&word| initial[word] == 0.0) {
        let mut changed = initial;
        changed[word] = -0.0;
        let mut slot = Mat4::IDENTITY;
        assert!(update_joint_matrix(
            &mut slot,
            Mat4::from_cols_array(&changed)
        ));
        assert_eq!(
            slot.to_cols_array().map(f32::to_bits),
            changed.map(f32::to_bits)
        );
        assert!(!update_joint_matrix(
            &mut slot,
            Mat4::from_cols_array(&changed)
        ));
    }
}

#[test]
fn identical_upload_bits_reuse_even_with_a_nan_payload() {
    let mut words = Mat4::IDENTITY.to_cols_array();
    words[0] = f32::from_bits(0x7fc0_0001);
    let matrix = Mat4::from_cols_array(&words);
    let mut slot = matrix;
    assert!(!update_joint_matrix(&mut slot, matrix));
    assert_eq!(
        slot.to_cols_array().map(f32::to_bits),
        words.map(f32::to_bits)
    );
}

#[test]
fn ordinary_joint_change_updates_then_settles() {
    let mut words = Mat4::IDENTITY.to_cols_array();
    words[12] = 2.0;
    let matrix = Mat4::from_cols_array(&words);
    let mut slot = Mat4::IDENTITY;
    assert!(update_joint_matrix(&mut slot, matrix));
    assert_eq!(slot, matrix);
    assert!(!update_joint_matrix(&mut slot, matrix));
}
