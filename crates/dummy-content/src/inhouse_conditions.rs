//! Independently framed asymmetric CTDA fixtures, separate from the decoder schema.

/// Frame a native short subrecord using its independently supplied payload size.
pub fn subrecord(signature: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut bytes = signature.to_vec();
    bytes.extend(
        u16::try_from(payload.len())
            .expect("small fixture field")
            .to_le_bytes(),
    );
    bytes.extend(payload);
    bytes
}

/// Frame an SSE record with recognizable revision and padding sentinels.
pub fn record(signature: &[u8; 4], id: u32, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut bytes = signature.to_vec();
    bytes.extend(
        u32::try_from(payload.len())
            .expect("small fixture record")
            .to_le_bytes(),
    );
    bytes.extend(flags.to_le_bytes());
    bytes.extend(id.to_le_bytes());
    bytes.extend(0x5731_A902u32.to_le_bytes());
    bytes.extend(44u16.to_le_bytes());
    bytes.extend(0xC537u16.to_le_bytes());
    bytes.extend(payload);
    bytes
}

/// Encode the native32-byte condition with unequal unused bytes and parameters.
pub fn condition(
    flags: u8,
    comparison: u32,
    function: u16,
    first: u32,
    second: u32,
    run_on: u32,
    reference: u32,
) -> Vec<u8> {
    let mut bytes = vec![flags, 0x91, 0x37, 0xD5];
    bytes.extend(comparison.to_le_bytes());
    bytes.extend(function.to_le_bytes());
    bytes.extend([0xCA, 0x53]);
    bytes.extend(first.to_le_bytes());
    bytes.extend(second.to_le_bytes());
    bytes.extend(run_on.to_le_bytes());
    bytes.extend(reference.to_le_bytes());
    bytes.extend((-17i32).to_le_bytes());
    bytes
}

/// A complete native spell whose condition occurrences may include malformed CTDA.
pub fn spell(id: u32, fields: &[u8]) -> Vec<u8> {
    let mut payload = subrecord(b"EDID", b"IndependentConditions\0");
    let mut data = vec![0; 36];
    data[..4].copy_from_slice(&71u32.to_le_bytes());
    data[12..16].copy_from_slice(&1.75f32.to_le_bytes());
    data[24..28].copy_from_slice(&3.25f32.to_le_bytes());
    data[28..32].copy_from_slice(&19.5f32.to_le_bytes());
    payload.extend(subrecord(b"SPIT", &data));
    payload.extend(fields);
    record(b"SPEL", id, 0, &payload)
}

/// A native global supplies a precisely typed comparison or function argument.
pub fn global(id: u32) -> Vec<u8> {
    let mut payload = subrecord(b"EDID", b"ConditionGlobal\0");
    payload.extend(subrecord(b"FNAM", b"f"));
    payload.extend(subrecord(b"FLTV", &13.75f32.to_le_bytes()));
    record(b"GLOB", id, 0, &payload)
}

/// A synthetic placed reference supplies the exact target category for CTDA links.
pub fn reference(id: u32) -> Vec<u8> {
    reference_with_base(id, 0x14)
}

/// Supply a file-local base independently of the placed reference's own namespace.
pub fn reference_with_base(id: u32, base: u32) -> Vec<u8> {
    let mut payload = subrecord(b"NAME", &base.to_le_bytes());
    let mut transform = Vec::new();
    for scalar in [17.25f32, -23.5, 31.75, 0.31, -0.27, 0.79] {
        transform.extend(scalar.to_le_bytes());
    }
    payload.extend(subrecord(b"DATA", &transform));
    record(b"REFR", id, 0, &payload)
}
