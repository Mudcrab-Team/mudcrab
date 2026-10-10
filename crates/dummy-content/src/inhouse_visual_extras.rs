//! Independently framed asymmetric weather, material and visual fixtures.

/// Stable native fixture identifiers in signature order.
pub const FIRST_ID: u32 = 0x2800;
/// Every signature assigned to the visual family, including absent official LENS.
pub const SIGNATURES: [[u8; 4]; 15] = [
    *b"WTHR", *b"CLMT", *b"IMGS", *b"IMAD", *b"LGTM", *b"MATO", *b"MATT", *b"IPCT", *b"IPDS",
    *b"CAMS", *b"CPTH", *b"VOLI", *b"LENS", *b"SPGD", *b"RFCT",
];

/// Frame a native subrecord without using schema metadata.
pub fn subrecord(signature: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut bytes = signature.to_vec();
    bytes.extend(
        u16::try_from(payload.len())
            .expect("small fixture")
            .to_le_bytes(),
    );
    bytes.extend(payload);
    bytes
}

/// Frame a native44-version record, preserving asymmetric header metadata.
pub fn record(signature: &[u8; 4], id: u32, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut bytes = signature.to_vec();
    bytes.extend(
        u32::try_from(payload.len())
            .expect("small fixture")
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

/// Encode distinct finite floats so ordering and offsets have visible witnesses.
fn floats(count: usize, first: f32) -> Vec<u8> {
    (0..count)
        .flat_map(|i| (first + i as f32 * 0.375).to_le_bytes())
        .collect()
}

/// Build a native count-controlled IMAD with independently framed curve keys.
pub fn image_modifier(id: u32, blur_count: u32) -> Vec<u8> {
    let mut counts = vec![0u8; 244];
    counts[..4].copy_from_slice(&1u32.to_le_bytes());
    counts[4..8].copy_from_slice(&3.625f32.to_le_bytes());
    for offset in (8..176).step_by(4) {
        counts[offset..offset + 4].copy_from_slice(&2u32.to_le_bytes());
    }
    for offset in [
        176, 180, 184, 188, 192, 196, 212, 216, 220, 228, 232, 236, 240,
    ] {
        counts[offset..offset + 4].copy_from_slice(&2u32.to_le_bytes());
    }
    counts[180..184].copy_from_slice(&blur_count.to_le_bytes());
    counts[200..204].copy_from_slice(&0x8000_0001u32.to_le_bytes());
    counts[204..208].copy_from_slice(&0.3125f32.to_le_bytes());
    counts[208..212].copy_from_slice(&(-0.8125f32).to_le_bytes());
    counts[224..228].copy_from_slice(&[1, 0xA5, 0x73, 0xB2]);
    let mut payload = subrecord(b"EDID", b"IndependentImageAnimation\0");
    payload.extend(subrecord(b"DNAM", &counts));
    let ordinary = floats(4, -1.875);
    let color = floats(10, 0.4375);
    for sig in [
        b"BNAM", b"VNAM", b"TNAM", b"NAM3", b"RNAM", b"SNAM", b"UNAM", b"NAM1", b"NAM2", b"WNAM",
        b"XNAM", b"YNAM", b"NAM4",
    ] {
        payload.extend(subrecord(
            sig,
            if sig == b"TNAM" || sig == b"NAM3" {
                &color
            } else {
                &ordinary
            },
        ));
    }
    for index in 0u8..=20 {
        payload.extend(subrecord(&[index, b'I', b'A', b'D'], &ordinary));
        payload.extend(subrecord(&[index + 64, b'I', b'A', b'D'], &ordinary));
    }
    record(b"IMAD", id, 0, &payload)
}

/// Build all15 types with native sizes, contextual tags, links and optional tails.
pub fn records() -> Vec<u8> {
    let mut result = Vec::new();
    for (index, signature) in SIGNATURES.iter().enumerate() {
        let id = FIRST_ID + u32::try_from(index).expect("fixture index");
        if signature == b"IMAD" {
            result.extend(image_modifier(id, 2));
            continue;
        }
        let mut payload = subrecord(b"EDID", b"IndependentVisualFixture\0");
        match signature {
            b"WTHR" => {
                for layer in 0u8..29 {
                    let first = if layer < 17 {
                        b'0' + layer
                    } else {
                        b'A' + layer - 17
                    };
                    payload.extend(subrecord(
                        &[first, b'0', b'T', b'X'],
                        b"independent/cloud.dds\0",
                    ));
                }
                payload.extend(subrecord(b"LNAM", &[0x73, 0xB2, 0xA5, 0xE9]));
                payload.extend(subrecord(b"MNAM", &(FIRST_ID + 13).to_le_bytes()));
                payload.extend(subrecord(b"NNAM", &(FIRST_ID + 14).to_le_bytes()));
                payload.extend(subrecord(b"RNAM", &(0u8..32).collect::<Vec<_>>()));
                payload.extend(subrecord(b"QNAM", &(32u8..64).collect::<Vec<_>>()));
                payload.extend(subrecord(
                    b"PNAM",
                    &(0u16..512).map(|x| (x % 251) as u8).collect::<Vec<_>>(),
                ));
                payload.extend(subrecord(b"JNAM", &floats(128, 0.0625)));
                payload.extend(subrecord(b"NAM0", &[0xA5; 224]));
                payload.extend(subrecord(b"FNAM", &floats(8, 13.625)));
                let mut data = (0u8..19).collect::<Vec<_>>();
                data[11] = 0xA5;
                payload.extend(subrecord(b"DATA", &data));
                payload.extend(subrecord(b"NAM1", &0x8010_0021u32.to_le_bytes()));
                let mut images = Vec::new();
                let mut lighting = Vec::new();
                for _ in 0..4 {
                    images.extend((FIRST_ID + 2).to_le_bytes());
                    lighting.extend((FIRST_ID + 11).to_le_bytes());
                }
                payload.extend(subrecord(b"IMSP", &images));
                payload.extend(subrecord(b"HNAM", &lighting));
                for _ in 0..4 {
                    payload.extend(subrecord(b"DALC", &[0x73; 32]));
                }
                payload.extend(subrecord(b"GNAM", &(FIRST_ID + 12).to_le_bytes()));
            }
            b"CLMT" => {
                let mut choice = FIRST_ID.to_le_bytes().to_vec();
                choice.extend((-17i32).to_le_bytes());
                choice.extend(0u32.to_le_bytes());
                payload.extend(subrecord(b"WLST", &choice));
                payload.extend(subrecord(b"FNAM", b"sun.dds\0"));
                payload.extend(subrecord(b"GNAM", b"glare.dds\0"));
                payload.extend(subrecord(b"TNAM", &[7, 13, 19, 23, 31, 0xA5]));
            }
            b"IMGS" => {
                payload.extend(subrecord(b"HNAM", &floats(9, 0.625)));
                payload.extend(subrecord(b"CNAM", &floats(3, 1.375)));
                payload.extend(subrecord(b"TNAM", &floats(4, 0.1875)));
                let mut dof = floats(3, 7.875);
                dof.extend([0xA5, 0x73]);
                dof.extend(16984u16.to_le_bytes());
                payload.extend(subrecord(b"DNAM", &dof));
            }
            b"LGTM" => {
                let mut light = vec![0x73; 92];
                light[12..20].copy_from_slice(&floats(2, 17.125));
                light[20..24].copy_from_slice(&(-37i32).to_le_bytes());
                light[24..28].copy_from_slice(&83i32.to_le_bytes());
                light[28..40].copy_from_slice(&floats(3, 0.3125));
                light[68..72].copy_from_slice(&0.875f32.to_le_bytes());
                light[76..88].copy_from_slice(&floats(3, 31.125));
                payload.extend(subrecord(b"DATA", &light));
                payload.extend(subrecord(b"DALC", &[0xA5; 24]));
            }
            b"MATO" => {
                payload.extend(subrecord(b"DNAM", &[0xA5, 0x73, 0xB2]));
                payload.extend(subrecord(b"DNAM", &[0xC8, 0x39]));
                let mut data = floats(11, 0.3125);
                data.extend(0x8000_0001u32.to_le_bytes());
                data.extend([0xA5, 0x73, 0xB2, 0xE9]);
                payload.extend(subrecord(b"DATA", &data));
            }
            b"MATT" => {
                payload.extend(subrecord(b"MNAM", b"IndependentMaterial\0"));
                payload.extend(subrecord(b"CNAM", &floats(3, 0.1875)));
                payload.extend(subrecord(b"BNAM", &0.625f32.to_le_bytes()));
                payload.extend(subrecord(b"FNAM", &0x8000_0003u32.to_le_bytes()));
                payload.extend(subrecord(b"HNAM", &(FIRST_ID + 8).to_le_bytes()));
            }
            b"IPCT" => {
                let mut data = 1.625f32.to_le_bytes().to_vec();
                data.extend(2u32.to_le_bytes());
                data.extend(floats(2, 0.4375));
                data.extend(3u32.to_le_bytes());
                data.extend([0xA5, 4, 0x73, 0xB2]);
                payload.extend(subrecord(b"DATA", &data));
                let mut decal = floats(7, 0.8125);
                decal.extend([7, 0xA5, 0x73, 0xB2, 17, 31, 43, 0xE9]);
                payload.extend(subrecord(b"DODT", &decal));
            }
            b"IPDS" => {
                for _ in 0..2 {
                    let mut data = (FIRST_ID + 6).to_le_bytes().to_vec();
                    data.extend((FIRST_ID + 7).to_le_bytes());
                    payload.extend(subrecord(b"PNAM", &data));
                }
            }
            b"CAMS" => {
                let mut data = Vec::new();
                for word in [2u32, 1, 3, 0x8000_0025] {
                    data.extend(word.to_le_bytes());
                }
                data.extend(floats(7, 0.1875));
                payload.extend(subrecord(b"DATA", &data));
                payload.extend(subrecord(b"MNAM", &(FIRST_ID + 3).to_le_bytes()));
            }
            b"CPTH" => {
                payload.extend(subrecord(b"CTDA", &[0xA5; 32]));
                let mut related = id.to_le_bytes().to_vec();
                related.extend(0u32.to_le_bytes());
                payload.extend(subrecord(b"ANAM", &related));
                payload.extend(subrecord(b"DATA", &[130]));
                for _ in 0..2 {
                    payload.extend(subrecord(b"SNAM", &(FIRST_ID + 9).to_le_bytes()));
                }
            }
            b"VOLI" => {
                for (index, tag) in [
                    b"CNAM", b"DNAM", b"ENAM", b"FNAM", b"GNAM", b"HNAM", b"INAM", b"JNAM",
                    b"KNAM", b"LNAM", b"MNAM", b"NNAM",
                ]
                .iter()
                .enumerate()
                {
                    payload.extend(subrecord(
                        tag,
                        &(0.1875 + index as f32 * 0.0625).to_le_bytes(),
                    ));
                }
            }
            b"LENS" => {
                payload.extend(subrecord(b"CNAM", &0.4375f32.to_le_bytes()));
                payload.extend(subrecord(b"DNAM", &1.8125f32.to_le_bytes()));
                payload.extend(subrecord(b"LFSP", &2u32.to_le_bytes()));
                for name in [b"first\0".as_slice(), b"second\0".as_slice()] {
                    payload.extend(subrecord(b"DNAM", name));
                    payload.extend(subrecord(b"FNAM", b"independent/lens.dds\0"));
                    let mut data = floats(8, 0.3125);
                    data.extend(0x8000_0003u32.to_le_bytes());
                    payload.extend(subrecord(b"LFSD", &data));
                }
            }
            b"SPGD" => {
                let mut data = floats(7, -0.1875);
                for word in [7u32, 11, 1, 37] {
                    data.extend(word.to_le_bytes());
                }
                data.extend(0.8125f32.to_le_bytes());
                payload.extend(subrecord(b"DATA", &data));
                payload.extend(subrecord(b"ICON", b"independent/particle.dds\0"));
            }
            b"RFCT" => {
                let mut data = vec![0; 8];
                data.extend(0x8000_0005u32.to_le_bytes());
                payload.extend(subrecord(b"DATA", &data));
            }
            _ => unreachable!("exhaustive native visual fixture"),
        }
        result.extend(record(signature, id, 0, &payload));
    }
    result
}
