//! Independently framed PERK fixtures with asymmetric discriminators and native padding.

/// Stable fixture identity, outside the phase-two family ranges.
pub const FIRST_ID: u32 = 0x2600;
/// A quest target is opaque until the separate dialogue family is integrated.
pub const QUEST_ID: u32 = FIRST_ID + 0xF0;
/// Spell target used by ability, selection and activation alternatives.
pub const SPELL_ID: u32 = FIRST_ID + 0xF1;
/// Leveled-item target used by the distinct list alternative.
pub const LIST_ID: u32 = FIRST_ID + 0xF2;

/// Write a native six-byte subrecord header without consulting reader schemas.
pub fn subrecord(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut bytes = tag.to_vec();
    bytes.extend(
        u16::try_from(payload.len())
            .expect("small fixture field")
            .to_le_bytes(),
    );
    bytes.extend(payload);
    bytes
}

/// Write a 24-byte SSE record, including nonzero version/padding sentinels.
pub fn record(tag: &[u8; 4], id: u32, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut bytes = tag.to_vec();
    bytes.extend(
        u32::try_from(payload.len())
            .expect("small fixture record")
            .to_le_bytes(),
    );
    bytes.extend(flags.to_le_bytes());
    bytes.extend(id.to_le_bytes());
    bytes.extend(0x5739_A7C1u32.to_le_bytes());
    bytes.extend(44u16.to_le_bytes());
    bytes.extend(0x39B7u16.to_le_bytes());
    bytes.extend(payload);
    bytes
}

/// Encode author-written NUL-terminated text.
pub fn text(value: &str) -> Vec<u8> {
    let mut bytes = value.as_bytes().to_vec();
    bytes.push(0);
    bytes
}

/// Begin a complete perk with a valid five-byte top-level DATA payload.
pub fn prefix(level: u8) -> Vec<u8> {
    let mut payload = subrecord(b"EDID", &text("IndependentPerk"));
    payload.extend(subrecord(b"FULL", &text("Asymmetric perk")));
    payload.extend(subrecord(b"DESC", &text("Independent description")));
    payload.extend(subrecord(b"ICON", &text("icons/large-perk.dds")));
    payload.extend(subrecord(b"MICO", &text("icons/small-perk.dds")));
    payload.extend(subrecord(b"DATA", &[1, level, 3, 0, 1]));
    payload
}

/// A quest effect includes a one-byte stage followed by three untouched padding bytes.
pub fn quest_effect(quest: u32) -> Vec<u8> {
    let mut data = quest.to_le_bytes().to_vec();
    data.extend([37, 0xA7, 0x53, 0xC1]);
    let mut payload = subrecord(b"PRKE", &[0, 2, 9]);
    payload.extend(subrecord(b"DATA", &data));
    payload.extend(subrecord(b"PRKF", &[]));
    payload
}

/// An ability effect uses the same four-byte width as several parameter alternatives.
pub fn ability_effect(spell: u32) -> Vec<u8> {
    [
        subrecord(b"PRKE", &[1, 1, 17]),
        subrecord(b"DATA", &spell.to_le_bytes()),
        subrecord(b"PRKF", &[]),
    ]
    .concat()
}

/// An entry function carries its own discriminator and optionally a label/fragment selector.
pub fn entry_effect(
    entry_point: u8,
    function: u8,
    parameter_type: u8,
    parameters: Option<&[u8]>,
    label: Option<&[u8]>,
) -> Vec<u8> {
    let mut payload = subrecord(b"PRKE", &[2, 4, 23]);
    payload.extend(subrecord(b"DATA", &[entry_point, function, 0]));
    payload.extend(subrecord(b"EPFT", &[parameter_type]));
    if let Some(label) = label {
        payload.extend(subrecord(b"EPF2", label));
        let mut flags = 0x8003u16.to_le_bytes().to_vec();
        flags.extend(0x0B17u16.to_le_bytes());
        payload.extend(subrecord(b"EPF3", &flags));
    }
    if let Some(parameters) = parameters {
        payload.extend(subrecord(b"EPFD", parameters));
    }
    payload.extend(subrecord(b"PRKF", &[]));
    payload
}

/// Build all three effect kinds and every function, including none/text/actor-value variants.
pub fn complete(id: u32) -> Vec<u8> {
    let mut payload = prefix(23);
    payload.extend(subrecord(b"NNAM", &id.to_le_bytes()));
    payload.extend(quest_effect(QUEST_ID));
    payload.extend(ability_effect(SPELL_ID));
    for function in 0..=15 {
        let (parameter_type, parameters) = match function {
            0 => (0, Some(vec![0xA7, 0x53])),
            1 => (1, Some(3.375f32.to_le_bytes().to_vec())),
            2 => (1, Some((-7.75f32).to_le_bytes().to_vec())),
            3 => (1, Some(2.125f32.to_le_bytes().to_vec())),
            4 => (
                2,
                Some([(-3.75f32).to_le_bytes(), 17.125f32.to_le_bytes()].concat()),
            ),
            5 | 12 | 13 | 14 => {
                let (actor_value, multiplier) = match function {
                    5 => (24u32, 1.875f32),
                    12 => (7, -1.125),
                    13 => (42, 2.25),
                    _ => (47, 0.625),
                };
                (
                    2,
                    Some([actor_value.to_le_bytes(), multiplier.to_le_bytes()].concat()),
                )
            }
            6 | 7 => (0, None),
            8 => (3, Some(LIST_ID.to_le_bytes().to_vec())),
            9 => (4, Some(SPELL_ID.to_le_bytes().to_vec())),
            10 => (5, Some(SPELL_ID.to_le_bytes().to_vec())),
            11 => (6, Some(text("Actor graph variable"))),
            15 => (7, Some(text("Distinct activation text"))),
            _ => unreachable!("bounded function sequence"),
        };
        let label = text("Run independent choice");
        payload.extend(entry_effect(
            85 - function,
            function,
            parameter_type,
            parameters.as_deref(),
            (function == 9).then_some(label.as_slice()),
        ));
    }
    record(b"PERK", id, 0, &payload)
}

/// Emit every assigned entry-point number so range coverage does not depend on vanilla use.
pub fn all_entry_points(id: u32) -> Vec<u8> {
    let mut payload = prefix(31);
    for entry_point in 0..=91 {
        let parameters = (f32::from(entry_point) + 0.375).to_le_bytes();
        payload.extend(entry_effect(entry_point, 1, 1, Some(&parameters), None));
    }
    record(b"PERK", id, 0, &payload)
}

/// Supply real target kinds while keeping fixture target payloads independent of perk schemas.
pub fn targets() -> Vec<u8> {
    [
        record(
            b"QUST",
            QUEST_ID,
            0,
            &subrecord(b"EDID", &text("SyntheticQuestTarget")),
        ),
        record(
            b"SPEL",
            SPELL_ID,
            0,
            &subrecord(b"EDID", &text("SyntheticSpellTarget")),
        ),
        record(
            b"LVLI",
            LIST_ID,
            0,
            &subrecord(b"EDID", &text("SyntheticListTarget")),
        ),
    ]
    .concat()
}
