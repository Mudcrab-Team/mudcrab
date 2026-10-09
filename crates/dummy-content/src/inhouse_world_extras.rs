//! Independently framed asymmetric world/navigation and deprecated-type fixtures.
use crate::inhouse_magic::{record, subrecord};

/// Stable family identities; links deliberately differ from array and plugin positions.
pub const FIRST_ID: u32 = 0x2400;
/// One fixture for every world owner signature, including reference-known EDID-only types.
pub const SIGNATURES: [[u8; 4]; 11] = [
    *b"NAVM", *b"NAVI", *b"REGN", *b"ECZN", *b"SCOL", *b"PLYR", *b"CLDC", *b"HAIR", *b"PWAT",
    *b"RGDL", *b"SCPT",
];

/// Encode source words without schema-derived framing or offsets.
fn words(values: &[u32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

/// Encode three unequal coordinates per source vertex.
fn floats(values: &[f32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

/// The parent is either a CELL link or caller-specified signed Y/X coordinate bits.
fn pathing_cell(world: u32, parent: u32) -> Vec<u8> {
    words(&[0xA502_7351, world, parent])
}

/// NAVM geometry has four grid cells with empty, one- and two-triangle lists.
pub fn geometry(world: u32, parent: u32, navmesh: u32, door: u32) -> Vec<u8> {
    let mut bytes = words(&[12]);
    bytes.extend(pathing_cell(world, parent));
    bytes.extend(words(&[3]));
    bytes.extend(floats(&[
        1.25, -2.5, 3.75, 7.5, 11.25, -13.5, -17.75, 19.5, 23.25,
    ]));
    bytes.extend(words(&[1]));
    for value in [0i16, 1, 2, -1, -2, -3] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.extend(0x0645u16.to_le_bytes());
    bytes.extend(0xA753u16.to_le_bytes());
    bytes.extend(words(&[1, 3, navmesh]));
    bytes.extend((-7i16).to_le_bytes());
    bytes.extend(words(&[1]));
    bytes.extend(0i16.to_le_bytes());
    bytes.extend(words(&[0xC103_3987, door]));
    bytes.extend(words(&[1]));
    bytes.extend(0i16.to_le_bytes());
    bytes.extend(words(&[2]));
    bytes.extend(floats(&[
        31.25, 47.5, -17.75, -2.5, -13.5, 7.5, 19.5, 23.25,
    ]));
    for values in [&[0i16, -3][..], &[][..], &[7][..], &[-2][..]] {
        bytes.extend(words(&[values.len() as u32]));
        for value in values {
            bytes.extend(value.to_le_bytes());
        }
    }
    bytes
}

/// All five counted lists and the square grid may be genuinely empty.
pub fn empty_geometry(world: u32, parent: u32) -> Vec<u8> {
    let mut bytes = words(&[12]);
    bytes.extend(pathing_cell(world, parent));
    bytes.extend(words(&[0, 0, 0, 0, 0, 0]));
    bytes.extend(floats(&[
        31.25, 47.5, -17.75, -2.5, -13.5, 7.5, 19.5, 23.25,
    ]));
    bytes
}

/// Construct a complete NAVM with connectors and a caller-chosen contextual parent.
pub fn navmesh(id: u32, world: u32, parent: u32, nav: u32, door: u32) -> Vec<u8> {
    let mut payload = subrecord(b"EDID", b"IndependentNavmesh\0");
    payload.extend(subrecord(b"NVNM", &geometry(world, parent, nav, door)));
    payload.extend(subrecord(b"ONAM", &words(&[3])));
    payload.extend(subrecord(b"PNAM", &[2, 0, 1, 0]));
    payload.extend(subrecord(b"NNAM", &[7, 0]));
    record(b"NAVM", id, 0, &payload)
}

/// NAVI info exercises both island branches and preserves nonzero opaque merge bytes.
pub fn info(navmesh: u32, island: bool, world: u32, parent: u32, door: u32) -> Vec<u8> {
    let mut bytes = words(&[navmesh, if island { 32 } else { 64 }]);
    bytes.extend(floats(&[7.25, -11.5, 13.75]));
    bytes.extend([0xA7, 0x53, 0xC1, 0x39]);
    bytes.extend(words(&[
        2,
        navmesh,
        navmesh,
        1,
        navmesh,
        1,
        0xC103_3987,
        door,
    ]));
    bytes.push(u8::from(island));
    if island {
        bytes.extend(floats(&[-7.25, -11.5, -13.75, 17.25, 19.5, 23.75]));
        bytes.extend(words(&[1]));
        for value in [0i16, 1, 2] {
            bytes.extend(value.to_le_bytes());
        }
        bytes.extend(words(&[3]));
        bytes.extend(floats(&[
            2.25, -3.5, 5.75, 7.25, 11.5, -13.75, -17.25, 19.5, 23.75,
        ]));
    }
    bytes.extend(pathing_cell(world, parent));
    bytes
}

/// NAVI paths have unequal lengths, an empty path, and ordered road-marker indices.
pub fn navi(id: u32, nav: u32, door: u32) -> Vec<u8> {
    let mut payload = subrecord(b"EDID", b"IndependentInfoMap\0");
    payload.extend(subrecord(b"NVER", &words(&[12])));
    payload.extend(subrecord(b"NVMI", &info(nav, false, 1, 0x0011_FFFD, door)));
    payload.extend(subrecord(b"NVMI", &info(nav, true, 0, 0x10, door)));
    payload.extend(subrecord(
        b"NVPP",
        &words(&[3, 2, nav, nav, 0, 1, nav, 2, nav, 13, nav, 29]),
    ));
    payload.extend(subrecord(b"NVSI", &words(&[nav])));
    record(b"NAVI", id, 0, &payload)
}

/// REGN preserves two area/entry groups, scalar widths, unknown padding and links.
pub fn region(id: u32) -> Vec<u8> {
    let mut payload = subrecord(b"EDID", b"IndependentRegion\0");
    payload.extend(subrecord(b"RCLR", &[17, 43, 91, 0xA7]));
    payload.extend(subrecord(b"WNAM", &words(&[1])));
    for (falloff, point) in [(37u32, [1.25f32, -3.75]), (83, [7.5, 11.25])] {
        payload.extend(subrecord(b"RPLI", &words(&[falloff])));
        payload.extend(subrecord(b"RPLD", &floats(&point)));
    }
    payload.extend(subrecord(b"RDAT", &[2, 0, 0, 0, 1, 37, 0xA7, 0x53]));
    let mut object = words(&[3]);
    object.extend(13u16.to_le_bytes());
    object.extend([0xC1, 0x39]);
    object.extend(floats(&[0.375]));
    object.extend([17, 23, 59, 0xC5]);
    object.extend(29u16.to_le_bytes());
    object.extend(31u16.to_le_bytes());
    object.extend(floats(&[-7.5, 19.25, 2.375, 0.625, 1.75]));
    for value in [37u16, 43, 59] {
        object.extend(value.to_le_bytes());
    }
    object.extend([0xA7, 0x53, 0xC1, 0x39, 0x87, 0xB3]);
    payload.extend(subrecord(b"RDOT", &object));
    payload.extend(subrecord(b"RDAT", &[4, 0, 0, 0, 0, 83, 0xC1, 0x39]));
    payload.extend(subrecord(b"RDMP", b"Original map label\0"));
    record(b"REGN", id, 0, &payload)
}

/// A valid family baseline uses distinct count/coordinate/padding values throughout.
pub fn records() -> Vec<u8> {
    let mut bytes = navmesh(FIRST_ID, 1, 0x0011_FFFD, FIRST_ID, 0x11);
    bytes.extend(navi(FIRST_ID + 1, FIRST_ID, 0x11));
    bytes.extend(region(FIRST_ID + 2));
    for (index, signature) in SIGNATURES.iter().enumerate().skip(3) {
        let mut payload = subrecord(b"EDID", b"IndependentWorldExtra\0");
        match signature {
            b"ECZN" => {
                payload.extend(subrecord(
                    b"DATA",
                    &[0, 0, 0, 0, 0, 0, 0, 0, 0xFD, 7, 5, 0x7F],
                ));
            }
            b"SCOL" => {
                payload.extend(subrecord(b"OBND", &[0; 12]));
                payload.extend(subrecord(b"MODL", b"meshes/generated.nif\0"));
                for placement in [
                    [1.25, -3.75, 7.5, 0.13, -0.27, 0.59, 1.75],
                    [-11.25, 13.5, 17.75, -0.31, 0.47, -0.83, 0.625],
                ] {
                    payload.extend(subrecord(b"ONAM", &words(&[3])));
                    payload.extend(subrecord(b"DATA", &floats(&placement)));
                }
            }
            b"PLYR" => payload.extend(subrecord(b"PLYR", &words(&[7]))),
            _ => {}
        }
        bytes.extend(record(signature, FIRST_ID + index as u32, 0, &payload));
    }
    bytes
}
