//! Independently framed asymmetric fixtures for phase-3 audio and miscellaneous records.
use crate::inhouse_items::{record, text};

/// Stable signature order for publication and load-order tests.
pub const SIGNATURES: [[u8; 4]; 19] = [
    *b"SOUN", *b"SNDR", *b"SOPM", *b"SNCT", *b"MUSC", *b"MUST", *b"ASPC", *b"REVB", *b"FSTP",
    *b"FSTS", *b"DOBJ", *b"DEBR", *b"ADDN", *b"AVIF", *b"CLFM", *b"COLL", *b"ANIO", *b"TACT",
    *b"LSCR",
];

/// Encode native floating-point array values without consulting the schema.
fn floats(values: &[f32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

/// Encode native word-array values in their author-selected sequence.
fn words(values: &[u32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

/// Build a MUST variant with a discriminator and deliberately reordered native fields.
#[must_use]
pub fn music_track(id: u32, kind: u32, palette_target: u32) -> Vec<u8> {
    let mut fields: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"EDID", text("SyntheticMusicTrack")),
        (b"CNAM", kind.to_le_bytes().to_vec()),
    ];
    match kind {
        0x23f6_78c3 => {
            fields.push((b"SNAM", words(&[palette_target, 0, palette_target])));
            fields.push((b"DNAM", 3.625f32.to_le_bytes().to_vec()));
        }
        0x6ed7_e048 => {
            fields.push((b"FNAM", floats(&[1.375, 7.625, 18.875])));
            fields.push((b"ANAM", text("Music\\independent_track.wav")));
            fields.push((b"BNAM", text("Music\\independent_finale.wav")));
            fields.push((
                b"LNAM",
                [floats(&[2.375, 21.625]), 7u32.to_le_bytes().to_vec()].concat(),
            ));
            fields.push((b"FLTV", 31.125f32.to_le_bytes().to_vec()));
        }
        0xa1a9_c4d5 => {
            fields.push((b"DNAM", 2.875f32.to_le_bytes().to_vec()));
            fields.push((b"FLTV", 11.625f32.to_le_bytes().to_vec()));
        }
        _ => unreachable!("fixture supports the three native variants"),
    }
    // This count is harmless without conditions and follows a late physical position.
    fields.push((b"CITC", 0u32.to_le_bytes().to_vec()));
    record(b"MUST", id, &fields)
}

/// Build every assigned record with unequal signed/scalar values and repeated child data.
#[must_use]
pub fn records(first: u32) -> Vec<Vec<u8>> {
    SIGNATURES
        .iter()
        .enumerate()
        .map(|(index, tag)| {
            let id = first + index as u32;
            if tag == b"MUST" {
                return music_track(id, 0x6ed7_e048, id);
            }
            let mut fields: Vec<(&[u8; 4], Vec<u8>)> =
                vec![(b"EDID", text("IndependentAudioFamily"))];
            match tag {
                b"SOUN" => {
                    fields.push((b"OBND", vec![0; 12]));
                    fields.push((b"FNAM", vec![0xa7, 0x53]));
                    fields.push((b"SNDD", vec![0x19, 0x7d, 0x2f]));
                    fields.push((b"SDSC", (first + 1).to_le_bytes().to_vec()));
                }
                b"SNDR" => {
                    fields.push((b"CNAM", 0x1eef_540au32.to_le_bytes().to_vec()));
                    fields.push((b"GNAM", (first + 3).to_le_bytes().to_vec()));
                    fields.push((b"SNAM", id.to_le_bytes().to_vec()));
                    fields.push((b"ANAM", text("Sound\\asymmetric_one.wav")));
                    fields.push((b"ANAM", text("Sound\\asymmetric_two.wav")));
                    fields.push((b"ONAM", (first + 2).to_le_bytes().to_vec()));
                    fields.push((b"FNAM", text("Independent subtitle")));
                    fields.push((b"LNAM", vec![0xa3, 0x20, 0x7d, 0x51]));
                    fields.push((b"BNAM", vec![0xf3, 0xf9, 37, 11, 0x35, 0x01]));
                }
                b"SOPM" => {
                    fields.push((b"NAM1", vec![3, 0xa5, 0x71, 53]));
                    fields.push((b"MNAM", 1u32.to_le_bytes().to_vec()));
                    fields.push((b"ONAM", (1..=24).collect()));
                    fields.push((
                        b"ANAM",
                        [
                            vec![0x53, 0xa7, 0x91, 0x13],
                            floats(&[37.625, 957.125]),
                            vec![93, 71, 43, 19, 3, 0x81, 0x63, 0x29],
                        ]
                        .concat(),
                    ));
                }
                b"SNCT" => {
                    fields.push((b"FULL", text("Independent category")));
                    fields.push((b"FNAM", 3u32.to_le_bytes().to_vec()));
                    fields.push((b"PNAM", id.to_le_bytes().to_vec()));
                    fields.push((b"VNAM", 39123u16.to_le_bytes().to_vec()));
                    fields.push((b"UNAM", 17731u16.to_le_bytes().to_vec()));
                }
                b"MUSC" => {
                    fields.push((b"FNAM", 0x6du32.to_le_bytes().to_vec()));
                    fields.push((
                        b"PNAM",
                        [17u16.to_le_bytes(), 1234u16.to_le_bytes()].concat(),
                    ));
                    fields.push((b"WNAM", 7.375f32.to_le_bytes().to_vec()));
                    fields.push((b"TNAM", words(&[first + 5, 0, first + 5])));
                }
                b"ASPC" => {
                    fields.push((b"OBND", vec![0; 12]));
                    fields.push((b"SNAM", (first + 1).to_le_bytes().to_vec()));
                    fields.push((b"BNAM", (first + 7).to_le_bytes().to_vec()));
                }
                b"REVB" => fields.push((
                    b"DATA",
                    [
                        3917u16.to_le_bytes().to_vec(),
                        7139u16.to_le_bytes().to_vec(),
                        vec![0xf3, 0xe1, 0xfb, 0x07, 83, 37, 19, 53, 71, 0xa7],
                    ]
                    .concat(),
                )),
                b"FSTP" => {
                    fields.push((b"DATA", 0u32.to_le_bytes().to_vec()));
                    fields.push((b"ANAM", text("FootIndependentLeft")));
                }
                b"FSTS" => {
                    fields.push((b"XCNT", words(&[1, 2, 0, 1, 1])));
                    fields.push((b"DATA", words(&[first + 8; 5])));
                }
                b"DOBJ" => fields.push((
                    b"DNAM",
                    words(&[
                        u32::from_le_bytes(*b"ZZ17"),
                        first + 7,
                        u32::from_le_bytes(*b"AA93"),
                        first + 3,
                    ]),
                )),
                b"DEBR" => {
                    for (percent, path, flags) in [
                        (37u8, "Meshes\\small.nif", 1u8),
                        (91, "Meshes\\much_longer_model.nif", 0),
                    ] {
                        fields.push((b"DATA", [vec![percent], text(path), vec![flags]].concat()));
                        fields.push((b"MODT", vec![0x57, percent, 0xa3]));
                    }
                }
                b"ADDN" => {
                    fields.push((b"OBND", vec![0; 12]));
                    fields.push((b"MODL", text("Meshes\\independent_addon.nif")));
                    fields.push((b"DATA", (-193i32).to_le_bytes().to_vec()));
                    fields.push((b"SNAM", (first + 1).to_le_bytes().to_vec()));
                    fields.push((b"DNAM", [71u16.to_le_bytes(), 3u16.to_le_bytes()].concat()));
                }
                b"AVIF" => {
                    fields.push((b"FULL", text("Independent actor value")));
                    fields.push((b"DESC", text("Independent skill description")));
                    fields.push((b"ANAM", text("IAV")));
                    fields.push((b"CNAM", vec![0x51, 0xa7, 0x13, 0x97]));
                    fields.push((b"AVSK", floats(&[1.375, 2.625, 5.875, -9.125])));
                    for node in [17u32, 93] {
                        fields.push((b"PNAM", 0u32.to_le_bytes().to_vec()));
                        fields.push((b"FNAM", node.to_le_bytes().to_vec()));
                        fields.push((b"XNAM", (node + 3).to_le_bytes().to_vec()));
                        fields.push((b"YNAM", (node + 7).to_le_bytes().to_vec()));
                        fields.push((b"HNAM", 0.375f32.to_le_bytes().to_vec()));
                        fields.push((b"VNAM", 0.875f32.to_le_bytes().to_vec()));
                        fields.push((b"SNAM", id.to_le_bytes().to_vec()));
                        fields.push((b"CNAM", (node + 1).to_le_bytes().to_vec()));
                        fields.push((b"CNAM", (node + 2).to_le_bytes().to_vec()));
                        fields.push((b"INAM", node.to_le_bytes().to_vec()));
                    }
                }
                b"CLFM" => {
                    fields.push((b"FULL", text("Independent color")));
                    fields.push((b"CNAM", vec![17, 93, 211, 31]));
                    fields.push((b"FNAM", 1u32.to_le_bytes().to_vec()));
                }
                b"COLL" => {
                    fields.push((b"DESC", text("Independent collision layer")));
                    fields.push((b"BNAM", 93u32.to_le_bytes().to_vec()));
                    fields.push((b"FNAM", vec![71, 13, 197, 0xa3]));
                    fields.push((b"GNAM", 5u32.to_le_bytes().to_vec()));
                    fields.push((b"MNAM", text("IndependentLayer")));
                    fields.push((b"INTV", 2u32.to_le_bytes().to_vec()));
                    fields.push((b"CNAM", words(&[id, 0])));
                }
                b"ANIO" => {
                    fields.push((b"MODL", text("Meshes\\independent_animated.nif")));
                    fields.push((b"BNAM", text("IndependentUnload")));
                }
                b"TACT" => {
                    fields.push((b"OBND", vec![0; 12]));
                    fields.push((b"FULL", text("Independent talking activator")));
                    fields.push((b"MODL", text("Meshes\\independent_talking.nif")));
                    fields.push((b"PNAM", vec![0x93, 0x57]));
                    fields.push((b"SNAM", (first + 1).to_le_bytes().to_vec()));
                    fields.push((b"FNAM", vec![0x13, 0xa7, 0x91]));
                    fields.push((b"VNAM", 0u32.to_le_bytes().to_vec()));
                }
                b"LSCR" => {
                    fields.push((b"DESC", text("Independent loading screen description")));
                    fields.push((b"NNAM", 0u32.to_le_bytes().to_vec()));
                    fields.push((b"SNAM", 1.625f32.to_le_bytes().to_vec()));
                    fields.push((
                        b"RNAM",
                        [
                            (-73i16).to_le_bytes(),
                            19i16.to_le_bytes(),
                            137i16.to_le_bytes(),
                        ]
                        .concat(),
                    ));
                    fields.push((
                        b"ONAM",
                        [(-157i16).to_le_bytes(), 113i16.to_le_bytes()].concat(),
                    ));
                    fields.push((b"XNAM", floats(&[-7.375, 19.625, 31.875])));
                    fields.push((b"MOD2", text("Cameras\\independent_camera.nif")));
                }
                _ => unreachable!("exhaustive fixture signature set"),
            }
            record(tag, id, &fields)
        })
        .collect()
}
