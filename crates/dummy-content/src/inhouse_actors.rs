//! Independently synthesized actor record fixtures owned by phase-2 agent B.
//!
//! These writers emit native frames directly. They share no layout/selector
//! implementation with the converter and never read proprietary inputs.

/// Emit a six-byte native subrecord frame around independently chosen bytes.
#[must_use]
pub fn subrecord(signature: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    let mut output = signature.to_vec();
    output.extend(
        u16::try_from(bytes.len())
            .expect("small synthetic subrecord")
            .to_le_bytes(),
    );
    output.extend(bytes);
    output
}

/// Emit a 24-byte Skyrim SE record header; malformed payloads remain bounded.
#[must_use]
pub fn record(signature: &[u8; 4], id: u32, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut output = signature.to_vec();
    output.extend(
        u32::try_from(payload.len())
            .expect("small synthetic record")
            .to_le_bytes(),
    );
    output.extend(flags.to_le_bytes());
    output.extend(id.to_le_bytes());
    output.extend([0; 4]);
    output.extend(44u16.to_le_bytes());
    output.extend([0; 2]);
    output.extend(payload);
    output
}

/// Encode NUL-terminated synthetic text.
#[must_use]
pub fn text(value: &str) -> Vec<u8> {
    let mut output = value.as_bytes().to_vec();
    output.push(0);
    output
}

/// Append master metadata to a dummy-content TES4 header and supplemental records.
#[must_use]
pub fn plugin(generated: &[u8], masters: &[&str], flags: u32, records: &[u8]) -> Vec<u8> {
    let size = u32::from_le_bytes(generated[4..8].try_into().expect("dummy TES4 size")) as usize;
    let mut metadata = generated[24..24 + size].to_vec();
    for master in masters {
        metadata.extend(subrecord(b"MAST", &text(master)));
        metadata.extend(subrecord(b"DATA", &[0xa5; 8]));
    }
    let mut output = record(b"TES4", 0, flags, &metadata);
    output.extend(records);
    output
}

/// Encode asymmetric native floating point members.
#[must_use]
pub fn floats(values: &[f32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

/// Build the 24-byte NPC configuration, including signed offsets and level.
#[must_use]
pub fn npc_configuration(multiplied: bool) -> Vec<u8> {
    let mut output = (if multiplied { 0x81u32 } else { 1 })
        .to_le_bytes()
        .to_vec();
    output.extend((-21i16).to_le_bytes());
    output.extend(37i16.to_le_bytes());
    output.extend(1500i16.to_le_bytes());
    output.extend(6u16.to_le_bytes());
    output.extend(79u16.to_le_bytes());
    output.extend(115u16.to_le_bytes());
    output.extend((-3i16).to_le_bytes());
    output.extend(0x1201u16.to_le_bytes());
    output.extend(93i16.to_le_bytes());
    output.extend(17u16.to_le_bytes());
    output
}

/// Build the native 12-byte vendor location; signed selectors choose its value.
#[must_use]
pub fn vendor_location(kind: i32, value: u32) -> Vec<u8> {
    let mut output = kind.to_le_bytes().to_vec();
    output.extend(value.to_le_bytes());
    output.extend((-73i32).to_le_bytes());
    output
}

/// Build an inventory item and its owner-dependent extra data.
#[must_use]
pub fn inventory(item: u32, owner: u32, value: u32, count: i32) -> Vec<u8> {
    let mut entry = item.to_le_bytes().to_vec();
    entry.extend(count.to_le_bytes());
    let mut extra = owner.to_le_bytes().to_vec();
    extra.extend(value.to_le_bytes());
    extra.extend(0.625f32.to_le_bytes());
    [subrecord(b"CNTO", &entry), subrecord(b"COED", &extra)].concat()
}

/// One representative of each assigned actor type with distinct native members.
///
/// IDs are consecutive from `0x2400`. References may be added independently by
/// callers; these representatives emphasize scalar widths and ordered contexts.
#[must_use]
pub fn representatives() -> Vec<u8> {
    let mut class = vec![0xa1, 0xb2, 0xc3, 0xd4, 7, 83];
    class.extend(1u8..=18);
    class.extend(0.3125f32.to_le_bytes());
    class.extend(0x0123_4567u32.to_le_bytes());
    class.extend([3, 7, 11, 13]);
    let mut relation = vec![0; 8];
    relation.extend(5u16.to_le_bytes());
    relation.extend([0xa5, 0x80]);
    relation.extend([0; 4]);
    let mut body = vec![0; 84];
    body[..4].copy_from_slice(&0.375f32.to_le_bytes());
    body[4..12].copy_from_slice(&[0x51, 1, 29, 0xff, 43, 61, 7, 0]);
    body[20..24].copy_from_slice(&1.25f32.to_le_bytes());
    body[28..32].copy_from_slice(&(-17i32).to_le_bytes());
    body[44..68].copy_from_slice(&floats(&[1.0, -2.0, 3.0, 0.25, -0.5, 0.75]));
    body[76..80].copy_from_slice(&[13, 19, 0xa5, 0x5a]);
    body[80..84].copy_from_slice(&0.875f32.to_le_bytes());
    let mut crime = vec![1, 0];
    for value in [1301u16, 229, 47, 83, 0xa55a] {
        crime.extend(value.to_le_bytes());
    }
    crime.extend(0.4375f32.to_le_bytes());
    crime.extend(733u16.to_le_bytes());
    crime.extend(911u16.to_le_bytes());
    let kinds: [(&[u8; 4], Vec<u8>); 14] = [
        (b"NPC_", subrecord(b"ACBS", &npc_configuration(true))),
        (b"RACE", subrecord(b"PNAM", &0.375f32.to_le_bytes())),
        (b"CLAS", subrecord(b"DATA", &class)),
        (
            b"FACT",
            [
                subrecord(b"DATA", &0x0001_4041u32.to_le_bytes()),
                subrecord(b"CRVA", &crime),
            ]
            .concat(),
        ),
        (b"OTFT", subrecord(b"INAM", &[])),
        (
            b"HDPT",
            [
                subrecord(b"DATA", &[0x27]),
                subrecord(b"PNAM", &4u32.to_le_bytes()),
                subrecord(b"NAM0", &2u32.to_le_bytes()),
                subrecord(b"NAM1", b"actors\\asymmetric.tri\0"),
            ]
            .concat(),
        ),
        (
            b"EYES",
            [
                subrecord(b"ICON", b"textures\\asymmetric.dds\0"),
                subrecord(b"DATA", &[5]),
            ]
            .concat(),
        ),
        (b"VTYP", subrecord(b"DNAM", &[3])),
        (
            b"CSTY",
            [
                subrecord(b"CSGD", &floats(&[0.125, 0.375])),
                subrecord(b"CSME", &floats(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0])),
                subrecord(b"DATA", &5u32.to_le_bytes()),
            ]
            .concat(),
        ),
        (
            b"BPTD",
            [
                subrecord(b"BPTN", b"Left synthetic limb\0"),
                subrecord(b"BPNN", b"LeftNode\0"),
                subrecord(b"BPND", &body),
            ]
            .concat(),
        ),
        (b"RELA", subrecord(b"DATA", &relation)),
        (
            b"ASTP",
            [
                subrecord(b"MPRT", b"uncle\0"),
                subrecord(b"FCHT", b"niece\0"),
                subrecord(b"DATA", &1u32.to_le_bytes()),
            ]
            .concat(),
        ),
        (b"LCRT", subrecord(b"CNAM", &[19, 71, 133, 211])),
        (
            b"MOVT",
            [
                subrecord(b"MNAM", b"ASYM\0"),
                subrecord(
                    b"SPED",
                    &floats(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 0.25, 0.75, 1.25]),
                ),
                subrecord(b"INAM", &floats(&[0.375, 73.0, 1.125])),
            ]
            .concat(),
        ),
    ];
    kinds
        .into_iter()
        .enumerate()
        .flat_map(|(index, (kind, fields))| {
            let mut payload = subrecord(
                b"EDID",
                &text(&format!("Synthetic{}", String::from_utf8_lossy(kind))),
            );
            payload.extend(fields);
            record(
                kind,
                0x2400 + u32::try_from(index).expect("fourteen representatives"),
                0,
                &payload,
            )
        })
        .collect()
}
