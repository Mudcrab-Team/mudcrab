//! Independently synthesized magic/list record fixtures owned by phase-2 agent C.
//! Independently framed asymmetric magic records for schema and publication tests.

/// First fixture FormID; successive indices follow the documented family order.
pub const FIRST_ID: u32 = 0x1800;
/// Every assigned family, in the fixture's stable ID order.
pub const SIGNATURES: [[u8; 4]; 18] = [
    *b"SPEL", *b"MGEF", *b"ENCH", *b"SHOU", *b"WOOP", *b"LVLI", *b"LVLN", *b"LVSP", *b"FLST",
    *b"KYWD", *b"GLOB", *b"GMST", *b"EXPL", *b"PROJ", *b"HAZD", *b"ARTO", *b"EFSH", *b"DUAL",
];

/// Frame native six-byte subrecord headers without involving the reader schema.
pub fn subrecord(signature: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut bytes = signature.to_vec();
    bytes.extend(
        u16::try_from(payload.len())
            .expect("small synthetic field")
            .to_le_bytes(),
    );
    bytes.extend(payload);
    bytes
}

/// Frame a native SSE record; flags can express a genuine header-only deletion.
pub fn record(signature: &[u8; 4], id: u32, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut bytes = signature.to_vec();
    bytes.extend(
        u32::try_from(payload.len())
            .expect("small synthetic record")
            .to_le_bytes(),
    );
    bytes.extend(flags.to_le_bytes());
    bytes.extend(id.to_le_bytes());
    bytes.extend(0x1234_5678u32.to_le_bytes());
    bytes.extend(44u16.to_le_bytes());
    bytes.extend(0xA573u16.to_le_bytes());
    bytes.extend(payload);
    bytes
}

/// Encode a NUL-terminated author-written string.
pub fn text(value: &str) -> Vec<u8> {
    let mut bytes = value.as_bytes().to_vec();
    bytes.push(0);
    bytes
}

/// Build a typed spell with distinct scalar values and two ordered effects.
pub fn spell(id: u32, base_cost: u32) -> Vec<u8> {
    let mut data = Vec::new();
    for word in [
        base_cost,
        0x0088_0001,
        2,
        1.25f32.to_bits(),
        1,
        2,
        3.75f32.to_bits(),
        17.5f32.to_bits(),
        0,
    ] {
        data.extend(word.to_le_bytes());
    }
    let mut payload = subrecord(b"EDID", &text("IndependentMagicSpell"));
    payload.extend(subrecord(b"SPIT", &data));
    for (magnitude, area, duration) in [(3.25f32, 7u32, 11u32), (-8.5, 13, 19)] {
        payload.extend(subrecord(b"EFID", &(FIRST_ID + 1).to_le_bytes()));
        let mut parameters = magnitude.to_le_bytes().to_vec();
        parameters.extend(area.to_le_bytes());
        parameters.extend(duration.to_le_bytes());
        payload.extend(subrecord(b"EFIT", &parameters));
        payload.extend(subrecord(b"CTDA", &[0xA5; 32]));
        payload.extend(subrecord(b"CIS1", &text("ordered condition")));
    }
    record(b"SPEL", id, 0, &payload)
}

/// Build one effect whose associated slot is selected by the later archetype.
pub fn effect(id: u32, archetype: u32, associated: u32) -> Vec<u8> {
    let mut data = vec![0; 152];
    data[4..8].copy_from_slice(&13.75f32.to_le_bytes());
    data[8..12].copy_from_slice(&associated.to_le_bytes());
    data[12..16].copy_from_slice(&18i32.to_le_bytes());
    data[16..20].copy_from_slice(&(-1i32).to_le_bytes());
    data[22..24].copy_from_slice(&[0xA7, 0x53]);
    data[64..68].copy_from_slice(&archetype.to_le_bytes());
    data[104..108].copy_from_slice(&0.625f32.to_le_bytes());
    record(
        b"MGEF",
        id,
        0,
        &[
            subrecord(b"EDID", &text("IndependentEffect")),
            subrecord(b"DATA", &data),
            subrecord(b"SNDD", &[]),
        ]
        .concat(),
    )
}

/// Native optional-tail records use caller-provided whole members, including empty shaders.
pub fn data_record(signature: &[u8; 4], id: u32, data: &[u8]) -> Vec<u8> {
    record(
        signature,
        id,
        0,
        &[
            subrecord(b"EDID", &text("IndependentTail")),
            subrecord(b"DATA", data),
        ]
        .concat(),
    )
}

/// One meaningful record per C family, with source padding and ordered lists retained.
pub fn records() -> Vec<u8> {
    let mut bytes = spell(FIRST_ID, 93);
    bytes.extend(effect(FIRST_ID + 1, 12, 0x18F0));
    for (index, signature) in SIGNATURES.iter().enumerate().skip(2) {
        let id = FIRST_ID + index as u32;
        let editor_id = if signature == b"GMST" {
            "bRawBoolean"
        } else {
            "IndependentFamily"
        };
        let mut payload = subrecord(b"EDID", &text(editor_id));
        match signature {
            b"ENCH" => {
                let mut data = vec![0; 32];
                data[..4].copy_from_slice(&(-17i32).to_le_bytes());
                data[12..16].copy_from_slice(&29i32.to_le_bytes());
                data[20..24].copy_from_slice(&6u32.to_le_bytes());
                data[24..28].copy_from_slice(&0.75f32.to_le_bytes());
                payload.extend(subrecord(b"ENIT", &data));
            }
            b"SHOU" => {
                let mut data = (FIRST_ID + 4).to_le_bytes().to_vec();
                data.extend(FIRST_ID.to_le_bytes());
                data.extend(9.375f32.to_le_bytes());
                payload.extend(subrecord(b"SNAM", &data));
            }
            b"WOOP" => payload.extend(subrecord(b"TNAM", &text("Asymmetric translation"))),
            b"LVLI" | b"LVLN" | b"LVSP" => {
                payload.extend(subrecord(b"LVLD", &[23]));
                payload.extend(subrecord(b"LVLF", &[1]));
                payload.extend(subrecord(b"LLCT", &[1]));
                let target = match signature {
                    b"LVLI" => 0x18F1,
                    b"LVLN" => id,
                    _ => FIRST_ID,
                };
                let mut data = vec![0; 12];
                data[..2].copy_from_slice(&7u16.to_le_bytes());
                data[2..4].copy_from_slice(&[0xA7, 0x53]);
                data[4..8].copy_from_slice(&target.to_le_bytes());
                data[8..10].copy_from_slice(&13u16.to_le_bytes());
                data[10..12].copy_from_slice(&[0xC1, 0x39]);
                payload.extend(subrecord(b"LVLO", &data));
            }
            b"FLST" => {
                payload.extend(subrecord(b"LNAM", &(FIRST_ID + 4).to_le_bytes()));
                payload.extend(subrecord(b"LNAM", &FIRST_ID.to_le_bytes()));
            }
            b"KYWD" => payload.extend(subrecord(b"CNAM", &[17, 43, 91, 0xA5])),
            b"GLOB" => {
                payload.extend(subrecord(b"FNAM", b"s"));
                payload.extend(subrecord(b"FLTV", &13.75f32.to_le_bytes()));
            }
            b"GMST" => payload.extend(subrecord(b"DATA", &2u32.to_le_bytes())),
            b"EXPL" => payload.extend(subrecord(b"DATA", &[0; 40])),
            b"PROJ" => {
                let mut data = vec![0; 84];
                data[2..4].copy_from_slice(&64u16.to_le_bytes());
                data[8..12].copy_from_slice(&317.25f32.to_le_bytes());
                payload.extend(subrecord(b"DATA", &data));
            }
            b"HAZD" => {
                let mut data = vec![0; 40];
                data[4..8].copy_from_slice(&83.25f32.to_le_bytes());
                data[24..28].copy_from_slice(&FIRST_ID.to_le_bytes());
                payload.extend(subrecord(b"DATA", &data));
            }
            b"ARTO" => payload.extend(subrecord(b"DNAM", &2u32.to_le_bytes())),
            b"EFSH" => {
                let mut data = vec![0; 308];
                data[..4].copy_from_slice(&[0xA1, 0xB2, 0xC3, 0xD4]);
                data[16..20].copy_from_slice(&[11, 37, 83, 0xA5]);
                data[20..24].copy_from_slice(&1.375f32.to_le_bytes());
                payload.extend(subrecord(b"DATA", &data));
            }
            b"DUAL" => {
                let data = [
                    FIRST_ID + 13,
                    FIRST_ID + 12,
                    FIRST_ID + 16,
                    FIRST_ID + 15,
                    0,
                    5,
                ]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>();
                payload.extend(subrecord(b"DATA", &data));
            }
            _ => unreachable!("closed fixture signatures"),
        }
        bytes.extend(record(signature, id, 0, &payload));
    }
    bytes.extend(record(
        b"LIGH",
        0x18F0,
        0,
        &subrecord(b"EDID", &text("AssociatedLight")),
    ));
    bytes.extend(record(
        b"MISC",
        0x18F1,
        0,
        &subrecord(b"EDID", &text("LeveledItem")),
    ));
    bytes
}
