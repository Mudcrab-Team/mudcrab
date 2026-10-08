//! Independently synthesized item and crafting fields; no proprietary inputs.

/// Frame one subrecord using its supplied native bytes.
#[must_use]
pub fn subrecord(signature: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut bytes = signature.to_vec();
    bytes.extend(
        u16::try_from(payload.len())
            .expect("fixture subrecord fits u16")
            .to_le_bytes(),
    );
    bytes.extend(payload);
    bytes
}

/// Frame a synthetic SSE record with distinguishable header metadata.
#[must_use]
pub fn record(signature: &[u8; 4], id: u32, fields: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let payload: Vec<u8> = fields
        .iter()
        .flat_map(|(tag, bytes)| subrecord(tag, bytes))
        .collect();
    let mut bytes = signature.to_vec();
    bytes.extend(
        u32::try_from(payload.len())
            .expect("fixture record fits u32")
            .to_le_bytes(),
    );
    bytes.extend(0_u32.to_le_bytes());
    bytes.extend(id.to_le_bytes());
    bytes.extend(0x1941_9626_u32.to_le_bytes());
    bytes.extend(44_u16.to_le_bytes());
    bytes.extend(0x07a9_u16.to_le_bytes());
    bytes.extend(payload);
    bytes
}

/// Encode synthetic unlocalized inline text with its native terminating zero.
#[must_use]
pub fn text(value: &str) -> Vec<u8> {
    let mut bytes = value.as_bytes().to_vec();
    bytes.push(0);
    bytes
}

/// Build a dependent plugin from dummy-content's TES4 metadata, preserving its body boundary.
#[must_use]
pub fn dependent_plugin(generated: &[u8], masters: &[&str], records: &[Vec<u8>]) -> Vec<u8> {
    let width = u32::from_le_bytes(generated[4..8].try_into().expect("TES4 size")) as usize;
    let mut payload = generated[24..24 + width].to_vec();
    for master in masters {
        payload.extend(subrecord(b"MAST", &text(master)));
        payload.extend(subrecord(b"DATA", &[0xa5; 8]));
    }
    let mut plugin = record(b"TES4", 0, &[]);
    plugin[4..8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    plugin.extend(payload);
    for bytes in records {
        plugin.extend(bytes);
    }
    plugin
}

/// BOOK DATA uses a flags-controlled signed skill or spell link at byte four.
#[must_use]
pub fn book(id: u32, flags: u8, teaching: u32) -> Vec<u8> {
    let mut data = vec![flags, 0xff, 0xa7, 0x51];
    data.extend(teaching.to_le_bytes());
    data.extend(137_u32.to_le_bytes());
    data.extend(2.75_f32.to_le_bytes());
    record(
        b"BOOK",
        id,
        &[
            (b"EDID", text("SyntheticTeachingBook")),
            (b"FULL", text("Synthetic book")),
            (b"DESC", text("Independent synthetic book text")),
            (b"DATA", data),
            (b"CNAM", text("Synthetic description")),
        ],
    )
}

/// WEAP separates its packed damage and both flag words from SSE critical padding.
#[must_use]
pub fn weapon(id: u32, critical_effect: u32) -> Vec<u8> {
    let mut basic = 271_u32.to_le_bytes().to_vec();
    basic.extend(3.625_f32.to_le_bytes());
    basic.extend(43_u16.to_le_bytes());
    let mut data = vec![0; 100];
    data[0] = 9;
    data[1..4].copy_from_slice(&[0xa7, 0x51, 0xd2]);
    data[4..8].copy_from_slice(&1.375_f32.to_le_bytes());
    data[8..12].copy_from_slice(&2.625_f32.to_le_bytes());
    data[12..14].copy_from_slice(&0x0089_u16.to_le_bytes());
    data[14..16].copy_from_slice(&[0x39, 0xe1]);
    data[24..28].copy_from_slice(&[73, 32, 3, 17]);
    data[28..32].copy_from_slice(&37.25_f32.to_le_bytes());
    data[32..36].copy_from_slice(&809.5_f32.to_le_bytes());
    data[36..40].copy_from_slice(&2_u32.to_le_bytes());
    data[40..44].copy_from_slice(&0x2031_u32.to_le_bytes());
    data[76..80].copy_from_slice(&(-13_i32).to_le_bytes());
    data[88..92].copy_from_slice(&(-1_i32).to_le_bytes());
    data[96..100].copy_from_slice(&0.875_f32.to_le_bytes());
    let mut critical = 19_u16.to_le_bytes().to_vec();
    critical.extend([0xe1, 0x27]);
    critical.extend(1.625_f32.to_le_bytes());
    critical.push(1);
    critical.extend([0xa7, 0x51, 0xd2, 0x38, 0xc4, 0x19, 0x96]);
    critical.extend(critical_effect.to_le_bytes());
    critical.extend([0x2a, 0x7b, 0x91, 0xee]);
    record(
        b"WEAP",
        id,
        &[
            (b"EDID", text("SyntheticAsymmetricWeapon")),
            (b"DATA", basic),
            (b"DNAM", data),
            (b"CRDT", critical),
            (b"VNAM", 2_u32.to_le_bytes().to_vec()),
        ],
    )
}

/// One record of each family exercises declared native packed structures.
#[must_use]
pub fn minimal_family_records(first_id: u32) -> Vec<Vec<u8>> {
    let signatures = [
        *b"WEAP", *b"ARMO", *b"ARMA", *b"AMMO", *b"BOOK", *b"MISC", *b"KEYM", *b"INGR", *b"ALCH",
        *b"SLGM", *b"SCRL", *b"APPA", *b"CONT", *b"COBJ", *b"EQUP",
    ];
    signatures
        .iter()
        .enumerate()
        .map(|(position, signature)| {
            let id = first_id + position as u32;
            if signature == b"WEAP" {
                return weapon(id, 0);
            }
            if signature == b"BOOK" {
                return book(id, 1, (-17_i32) as u32);
            }
            let mut fields: Vec<(&[u8; 4], Vec<u8>)> = vec![(b"EDID", text("SyntheticItem"))];
            match signature {
                b"ARMO" => {
                    fields.push((
                        b"BOD2",
                        [0x1234_5678_u32.to_le_bytes(), 2_u32.to_le_bytes()].concat(),
                    ));
                    fields.push((
                        b"DATA",
                        [(-123_i32).to_le_bytes(), 4.625_f32.to_le_bytes()].concat(),
                    ));
                    fields.push((b"DNAM", 731_i32.to_le_bytes().to_vec()));
                }
                b"ARMA" => {
                    let mut data = vec![19, 37, 2, 3, 0xa7, 0x51, 83, 0xd2];
                    data.extend(0.375_f32.to_le_bytes());
                    fields.push((b"DNAM", data));
                }
                b"AMMO" => fields.push((
                    b"DATA",
                    [
                        0_u32.to_le_bytes(),
                        5_u32.to_le_bytes(),
                        29.125_f32.to_le_bytes(),
                        811_u32.to_le_bytes(),
                        0.625_f32.to_le_bytes(),
                    ]
                    .concat(),
                )),
                b"MISC" | b"KEYM" | b"INGR" => {
                    fields.push((
                        b"DATA",
                        [(-137_i32).to_le_bytes(), 1.375_f32.to_le_bytes()].concat(),
                    ));
                    if signature == b"INGR" {
                        fields.push((
                            b"ENIT",
                            [(-271_i32).to_le_bytes(), 0x103_u32.to_le_bytes()].concat(),
                        ));
                    }
                }
                b"ALCH" => {
                    fields.push((b"DATA", 2.625_f32.to_le_bytes().to_vec()));
                    fields.push((
                        b"ENIT",
                        [
                            (-271_i32).to_le_bytes(),
                            0x20003_u32.to_le_bytes(),
                            0_u32.to_le_bytes(),
                            0.375_f32.to_le_bytes(),
                            0_u32.to_le_bytes(),
                        ]
                        .concat(),
                    ));
                }
                b"SLGM" => {
                    fields.push((
                        b"DATA",
                        [137_u32.to_le_bytes(), 0.625_f32.to_le_bytes()].concat(),
                    ));
                    fields.push((b"SOUL", vec![3]));
                    fields.push((b"SLCP", vec![5]));
                }
                b"SCRL" => {
                    fields.push((
                        b"DATA",
                        [137_u32.to_le_bytes(), 0.625_f32.to_le_bytes()].concat(),
                    ));
                    fields.push((b"SPIT", vec![0; 36]));
                }
                b"APPA" => {
                    fields.push((b"QUAL", 3_i32.to_le_bytes().to_vec()));
                    fields.push((
                        b"DATA",
                        [137_u32.to_le_bytes(), 0.625_f32.to_le_bytes()].concat(),
                    ));
                }
                b"CONT" => fields.push((
                    b"DATA",
                    [vec![5], 13.625_f32.to_le_bytes().to_vec()].concat(),
                )),
                b"COBJ" => fields.push((b"NAM1", 7_u16.to_le_bytes().to_vec())),
                b"EQUP" => fields.push((b"DATA", 1_u32.to_le_bytes().to_vec())),
                _ => unreachable!(),
            }
            record(signature, id, &fields)
        })
        .collect()
}
