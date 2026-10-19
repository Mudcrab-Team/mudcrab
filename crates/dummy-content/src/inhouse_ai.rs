//! Independently framed, asymmetric native AI fixtures for section E.

/// First fixture FormID; the family list fixes the successive record identities.
pub const FIRST_ID: u32 = 0x1C00;
/// All AI signatures owned by section E, in stable fixture order.
pub const SIGNATURES: [[u8; 4]; 4] = [*b"PACK", *b"IDLE", *b"IDLM", *b"AACT"];

/// Frame a native six-byte subrecord header independently of the decoder schema.
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

/// Frame a native SSE record, including asymmetric untouched header metadata.
pub fn record(signature: &[u8; 4], id: u32, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut bytes = signature.to_vec();
    bytes.extend(
        u32::try_from(payload.len())
            .expect("small fixture record")
            .to_le_bytes(),
    );
    bytes.extend(flags.to_le_bytes());
    bytes.extend(id.to_le_bytes());
    bytes.extend(0x1937_A5C2u32.to_le_bytes());
    bytes.extend(44u16.to_le_bytes());
    bytes.extend(0xA753u16.to_le_bytes());
    bytes.extend(payload);
    bytes
}

/// Encode an author-written NUL-terminated inline string.
pub fn text(value: &str) -> Vec<u8> {
    let mut bytes = value.as_bytes().to_vec();
    bytes.push(0);
    bytes
}

/// Native PLDT's signed discriminator and radius enclose one four-byte value.
pub fn location(kind: i32, value: u32, radius: i32) -> Vec<u8> {
    [
        kind.to_le_bytes(),
        value.to_le_bytes(),
        radius.to_le_bytes(),
    ]
    .concat()
}

/// Native PTDA uses the same envelope with a count/distance in its final slot.
pub fn target(kind: i32, value: u32, count: i32) -> Vec<u8> {
    [kind.to_le_bytes(), value.to_le_bytes(), count.to_le_bytes()].concat()
}

/// Native PDTO contains a u32 kind followed by a link or four-character subtype.
pub fn topic(kind: u32, value: [u8; 4]) -> Vec<u8> {
    [kind.to_le_bytes(), value].concat()
}

/// A package whose repeated input types and procedures cannot be collapsed by signature.
pub fn package(id: u32) -> Vec<u8> {
    let mut payload = subrecord(b"EDID", &text("IndependentPackage"));
    let mut configuration = 0x8002_0205u32.to_le_bytes().to_vec();
    configuration.extend([18, 3, 2, 0xA7]);
    configuration.extend(0x02A1u16.to_le_bytes());
    configuration.extend([0x53, 0xC9]);
    payload.extend(subrecord(b"PKDT", &configuration));
    let mut schedule = vec![0xFF, 9, 13, 21, 37, 0xA7, 0x53, 0xC9];
    schedule.extend((-97i32).to_le_bytes());
    payload.extend(subrecord(b"PSDT", &schedule));
    payload.extend(subrecord(b"IDLF", &[13]));
    payload.extend(subrecord(b"IDLC", &[2, 0xA7, 0x53, 0xC9]));
    payload.extend(subrecord(b"IDLT", &3.625f32.to_le_bytes()));
    payload.extend(subrecord(
        b"IDLA",
        &[(FIRST_ID + 1).to_le_bytes(), (FIRST_ID + 1).to_le_bytes()].concat(),
    ));
    payload.extend(subrecord(b"IDLB", &[0xA7, 0x53, 0xC9, 0x19]));
    payload.extend(subrecord(b"CNAM", &0u32.to_le_bytes()));
    payload.extend(subrecord(b"QNAM", &0u32.to_le_bytes()));
    payload.extend(subrecord(
        b"PKCU",
        &[7u32.to_le_bytes(), 0u32.to_le_bytes(), 93u32.to_le_bytes()].concat(),
    ));
    for (kind, value) in [
        ("Bool", vec![1]),
        ("Int", 0xF173_59A2u32.to_le_bytes().to_vec()),
        ("Float", (-3.625f32).to_le_bytes().to_vec()),
        ("ObjectList", 19.875f32.to_le_bytes().to_vec()),
    ] {
        payload.extend(subrecord(b"ANAM", &text(kind)));
        payload.extend(subrecord(b"CNAM", &value));
    }
    payload.extend(subrecord(b"ANAM", &text("Location")));
    payload.extend(subrecord(b"BNAM", &[0xA7, 0x53, 0xC9]));
    payload.extend(subrecord(b"PLDT", &location(8, (-19i32) as u32, -73)));
    payload.extend(subrecord(b"TPIC", &[0x31, 0x92]));
    payload.extend(subrecord(b"ANAM", &text("Target")));
    payload.extend(subrecord(b"PTDA", &target(4, (-23i32) as u32, 17)));
    payload.extend(subrecord(b"ANAM", &text("Topic")));
    payload.extend(subrecord(b"PDTO", &topic(0, 0u32.to_le_bytes())));
    payload.extend(subrecord(b"PDTO", &topic(1, *b"Helo")));
    for (index, name, flags) in [(-3i8, "Destination", 1u32), (7, "Count", 0x8000_0001)] {
        payload.extend(subrecord(b"UNAM", &index.to_le_bytes()));
        payload.extend(subrecord(b"BNAM", &text(name)));
        payload.extend(subrecord(b"PNAM", &flags.to_le_bytes()));
    }
    payload.extend(subrecord(b"XNAM", &[0xA7]));
    for (name, procedure, child_count) in [("Sequence", "Travel", 2u32), ("Procedure", "Wait", 0)] {
        payload.extend(subrecord(b"ANAM", &text(name)));
        payload.extend(subrecord(b"CITC", &0u32.to_le_bytes()));
        payload.extend(subrecord(
            b"PRCB",
            &[child_count.to_le_bytes(), 3u32.to_le_bytes()].concat(),
        ));
        payload.extend(subrecord(b"PNAM", &text(procedure)));
        payload.extend(subrecord(b"FNAM", &1u32.to_le_bytes()));
        for index in [1, 3] {
            payload.extend(subrecord(b"PKC2", &[index]));
        }
        for (set, clear, speed) in [(0x0002_0004u32, 0x0080_0000u32, 1u8), (0x0040, 0x0080, 3)] {
            let mut data = [set.to_le_bytes(), clear.to_le_bytes()].concat();
            data.extend(0x0021u16.to_le_bytes());
            data.extend(0x0080u16.to_le_bytes());
            data.extend([speed, 0xA7, 0x53, 0xC9]);
            payload.extend(subrecord(b"PFO2", &data));
        }
        payload.extend(subrecord(b"PFOR", &[0x93, 0x17, 0xC9]));
    }
    payload.extend(subrecord(b"UNAM", &[11]));
    payload.extend(subrecord(b"BNAM", &text("Public Input")));
    payload.extend(subrecord(b"PNAM", &1u32.to_le_bytes()));
    for marker in [b"POBA", b"POEA", b"POCA"] {
        payload.extend(subrecord(marker, &[]));
        payload.extend(subrecord(b"INAM", &(FIRST_ID + 1).to_le_bytes()));
        payload.extend(subrecord(b"SCHR", &[0xA7; 20]));
        if marker == b"POCA" {
            payload.extend(subrecord(b"SCDA", &[0x53; 17]));
        }
        payload.extend(subrecord(b"SCTX", &text("Unused legacy script")));
        // The permitted source declares these payloads unused bytes. The QNAM word
        // resembles a bad file-relative link and must remain untouched.
        payload.extend(subrecord(b"QNAM", &0xFEA7_53C9u32.to_le_bytes()));
        payload.extend(subrecord(b"TNAM", &[0x19, 0x73, 0xA7, 0x53]));
        payload.extend(subrecord(b"PDTO", &topic(1, *b"Purg")));
    }
    record(b"PACK", id, 0, &payload)
}

/// A six-byte legacy IDLE DATA and two distinct parent/previous-sibling links.
pub fn idle(id: u32) -> Vec<u8> {
    let mut payload = subrecord(b"EDID", &text("IndependentIdle"));
    payload.extend(subrecord(b"DNAM", &text("animations\\asymmetric.hkx")));
    payload.extend(subrecord(b"ENAM", &text("DistinctAnimationEvent")));
    payload.extend(subrecord(
        b"ANAM",
        &[(FIRST_ID + 3).to_le_bytes(), (FIRST_ID + 1).to_le_bytes()].concat(),
    ));
    payload.extend(subrecord(b"DATA", &[11, 37, 0x89, 5, 0x53, 0xA7]));
    record(b"IDLE", id, 0, &payload)
}

/// An idle marker retains a one-byte count, asymmetric bounds and ordered duplicate links.
pub fn idle_marker(id: u32) -> Vec<u8> {
    let mut payload = subrecord(b"EDID", &text("IndependentIdleMarker"));
    let bounds: Vec<u8> = [-13i16, -7, -3, 19, 29, 41]
        .into_iter()
        .flat_map(i16::to_le_bytes)
        .collect();
    payload.extend(subrecord(b"OBND", &bounds));
    payload.extend(subrecord(b"IDLF", &[0x15]));
    payload.extend(subrecord(b"IDLC", &[2]));
    payload.extend(subrecord(b"IDLT", &(-7.875f32).to_le_bytes()));
    payload.extend(subrecord(
        b"IDLA",
        &[(FIRST_ID + 1).to_le_bytes(), (FIRST_ID + 1).to_le_bytes()].concat(),
    ));
    record(b"IDLM", id, 0x2000_0000, &payload)
}

/// An AACT color deliberately resembles a nonzero FormID but remains four u8 values.
pub fn action(id: u32) -> Vec<u8> {
    let payload = [
        subrecord(b"EDID", &text("IndependentAction")),
        subrecord(b"CNAM", &[19, 73, 167, 211]),
    ]
    .concat();
    record(b"AACT", id, 0, &payload)
}

/// Build all assigned types using independently chosen values and native framing.
pub fn records() -> Vec<u8> {
    [
        package(FIRST_ID),
        idle(FIRST_ID + 1),
        idle_marker(FIRST_ID + 2),
        action(FIRST_ID + 3),
    ]
    .concat()
}
