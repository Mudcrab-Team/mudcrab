//! Native quest and dialogue fixtures authored independently of decoder layouts.

/// All eleven section-D record signatures, in a stable synthetic order.
pub const SIGNATURES: [[u8; 4]; 11] = [
    *b"QUST", *b"DIAL", *b"INFO", *b"DLBR", *b"DLVW", *b"SCEN", *b"SMBN", *b"SMQN", *b"SMEN",
    *b"MESG", *b"LCTN",
];

/// First local identifier used by the eleven representative records.
pub const FIRST_ID: u32 = 0x3400;

/// Frame one small subrecord directly, without production encoding helpers.
#[must_use]
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

/// Write the native 24-byte SSE record header with distinctive metadata.
#[must_use]
pub fn record(tag: &[u8; 4], id: u32, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut bytes = tag.to_vec();
    bytes.extend(
        u32::try_from(payload.len())
            .expect("small fixture record")
            .to_le_bytes(),
    );
    bytes.extend(flags.to_le_bytes());
    bytes.extend(id.to_le_bytes());
    bytes.extend(0x1739_5b7du32.to_le_bytes());
    bytes.extend(44u16.to_le_bytes());
    bytes.extend(0xa571u16.to_le_bytes());
    bytes.extend(payload);
    bytes
}

/// Encode synthetic zero-terminated ASCII text.
#[must_use]
pub fn text(value: &str) -> Vec<u8> {
    [value.as_bytes(), &[0]].concat()
}

/// Wrap independently chosen subrecords in a native record with an editor ID.
#[must_use]
pub fn named(tag: &[u8; 4], id: u32, fields: &[Vec<u8>]) -> Vec<u8> {
    record(
        tag,
        id,
        0,
        &[
            subrecord(b"EDID", &text(&format!("Dialogue{id:X}"))),
            fields.concat(),
        ]
        .concat(),
    )
}

/// Build a plugin using only the existing dummy TES4 metadata and chosen masters.
#[must_use]
pub fn plugin(generated: &[u8], masters: &[&str], flags: u32, records: &[u8]) -> Vec<u8> {
    let length = u32::from_le_bytes(generated[4..8].try_into().expect("TES4 size")) as usize;
    let mut header = generated[24..24 + length].to_vec();
    for master in masters {
        header.extend(subrecord(b"MAST", &text(master)));
        header.extend(subrecord(b"DATA", &[0xa5; 8]));
    }
    [record(b"TES4", 0, flags, &header), records.to_vec()].concat()
}

/// Emit GetLevel's parameter-free CTDA using a distinctive comparison value.
#[must_use]
pub fn condition(comparison: f32) -> Vec<u8> {
    let mut bytes = vec![0; 32];
    bytes[1..4].copy_from_slice(&[0xa5, 0x71, 0x29]);
    bytes[4..8].copy_from_slice(&comparison.to_le_bytes());
    bytes[8..10].copy_from_slice(&80u16.to_le_bytes());
    bytes[10..12].copy_from_slice(&[0xb7, 0x39]);
    subrecord(b"CTDA", &bytes)
}

/// Build all quest nesting boundaries with asymmetric stages, logs, and aliases.
#[must_use]
pub fn quest(id: u32) -> Vec<u8> {
    let mut general = vec![0x81, 0x80, 73, 44, 0xa1, 0xb2, 0xc3, 0xd4];
    general.extend(8u32.to_le_bytes());
    let mut fields = vec![
        subrecord(b"FULL", &text("Synthetic quest")),
        subrecord(b"DNAM", &general),
        subrecord(b"ENAM", b"ADDT"),
        condition(7.25),
        subrecord(b"NEXT", &[]),
        condition(13.5),
    ];
    for (stage, logs) in [
        (17u16, ["First log", "Second log"]),
        (93, ["Third log", "Fourth log"]),
    ] {
        let mut index = stage.to_le_bytes().to_vec();
        index.extend([2, 0xa9]);
        fields.push(subrecord(b"INDX", &index));
        for (number, log) in logs.into_iter().enumerate() {
            fields.extend([
                subrecord(b"QSDT", &[u8::try_from(number + 1).expect("log flag")]),
                condition(f32::from(stage) + number as f32 / 2.0),
                subrecord(b"CNAM", &text(log)),
                subrecord(b"NAM0", &0u32.to_le_bytes()),
            ]);
        }
    }
    for objective in [31u16, 79] {
        fields.extend([
            subrecord(b"QOBJ", &objective.to_le_bytes()),
            subrecord(b"FNAM", &1u32.to_le_bytes()),
            subrecord(b"NNAM", &text(&format!("Objective{objective}"))),
        ]);
        for alias in [-7i32, 29] {
            let mut target = alias.to_le_bytes().to_vec();
            target.extend([1, 0xa5, 0x71, 0x29]);
            fields.extend([subrecord(b"QSTA", &target), condition(f32::from(objective))]);
        }
    }
    fields.push(subrecord(b"ANAM", &83u32.to_le_bytes()));
    for (tag, alias, flags) in [
        (b"ALST", 7u32, vec![0x42, 0x80]),
        (b"ALLS", 19, vec![0x83, 0x41, 3, 0]),
        (b"ALST", 43, vec![0x04, 0x29, 1, 0]),
    ] {
        fields.extend([
            subrecord(tag, &alias.to_le_bytes()),
            subrecord(b"ALID", &text(&format!("Alias{alias}"))),
            subrecord(b"FNAM", &flags),
            subrecord(b"ALFI", &(-3i32).to_le_bytes()),
            condition(alias as f32 + 0.25),
            subrecord(b"KSIZ", &0u32.to_le_bytes()),
            subrecord(b"KWDA", &[]),
            subrecord(b"ALED", &[]),
        ]);
    }
    let mut target = 0u32.to_le_bytes().to_vec();
    target.extend([1, 0x29, 0x71, 0xa5]);
    fields.extend([
        subrecord(b"NNAM", &text("After aliases")),
        subrecord(b"QSTA", &target),
        condition(33.75),
    ]);
    named(b"QUST", id, &fields)
}

/// Build repeated response data with distinctive padding and native emotion values.
#[must_use]
pub fn response(id: u32, topic: u32, previous: u32) -> Vec<u8> {
    let mut fields = vec![
        subrecord(b"ENAM", &[1, 0xa0, 0xb7, 0x39]),
        subrecord(b"TPIC", &topic.to_le_bytes()),
        subrecord(b"PNAM", &previous.to_le_bytes()),
        subrecord(b"CNAM", &[3]),
    ];
    for (number, emotion, text_value) in
        [(3u8, 17u32, "First response"), (11, 73, "Second response")]
    {
        let mut data = 5u32.to_le_bytes().to_vec();
        data.extend(emotion.to_le_bytes());
        data.extend([0xa1, 0xb2, 0xc3, 0xd4, number, 0x29, 0x71, 0xa5]);
        data.extend(0u32.to_le_bytes());
        data.extend([1, 0x83, 0x41, 0x19]);
        fields.extend([
            subrecord(b"TRDT", &data),
            subrecord(b"NAM1", &text(text_value)),
            subrecord(b"NAM2", &text("Notes")),
            subrecord(b"NAM3", &text("Edits")),
            subrecord(b"SNAM", &0u32.to_le_bytes()),
            subrecord(b"LNAM", &0u32.to_le_bytes()),
        ]);
    }
    fields.extend([
        condition(21.25),
        subrecord(b"RNAM", &text("Prompt")),
        subrecord(b"ANAM", &0u32.to_le_bytes()),
        subrecord(b"TWAT", &0u32.to_le_bytes()),
        subrecord(b"ONAM", &0u32.to_le_bytes()),
    ]);
    named(b"INFO", id, &fields)
}

/// Build repeated phases/actors and every scene action variant in a differing order.
#[must_use]
pub fn scene(id: u32, quest: u32, topic: u32, package: u32) -> Vec<u8> {
    scene_with_root(id, quest, topic, package, 20, true)
}

/// Choose distinct root action metadata and the optional legacy script separator.
#[must_use]
pub fn scene_with_root(
    id: u32,
    quest: u32,
    topic: u32,
    package: u32,
    last_action_index: u32,
    script_separator: bool,
) -> Vec<u8> {
    let mut fields = vec![subrecord(b"FNAM", &0x15u32.to_le_bytes())];
    for (name, width) in [("Phase one", 179u32), ("Phase two", 263)] {
        fields.extend([
            subrecord(b"HNAM", &[]),
            subrecord(b"NAM0", &text(name)),
            condition(width as f32 + 0.25),
            subrecord(b"NEXT", &[]),
            condition(width as f32 + 0.75),
            subrecord(b"NEXT", &[]),
            subrecord(b"WNAM", &width.to_le_bytes()),
            subrecord(b"HNAM", &[]),
        ]);
    }
    for actor in [7u32, 29] {
        fields.extend([
            subrecord(b"ALID", &actor.to_le_bytes()),
            subrecord(b"LNAM", &3u32.to_le_bytes()),
            subrecord(b"DNAM", &26u32.to_le_bytes()),
        ]);
    }
    for (index, kind) in [2u16, 0, 1, 2].into_iter().enumerate() {
        fields.extend([
            subrecord(b"ANAM", &kind.to_le_bytes()),
            subrecord(b"NAM0", &text(&format!("Action{index}"))),
            subrecord(b"ALID", &(-7i32).to_le_bytes()),
            subrecord(b"LNAM", &[0xa5, 0x71]),
            subrecord(b"INAM", &(index as u32 + 17).to_le_bytes()),
            subrecord(b"FNAM", &0x28000u32.to_le_bytes()),
            subrecord(b"SNAM", &(index as u32 + 31).to_le_bytes()),
            subrecord(b"ENAM", &(index as u32 + 79).to_le_bytes()),
        ]);
        match kind {
            0 => fields.extend([
                subrecord(b"DATA", &topic.to_le_bytes()),
                subrecord(b"HTID", &(-19i32).to_le_bytes()),
                subrecord(b"DMAX", &3.75f32.to_le_bytes()),
                subrecord(b"DMIN", &0.625f32.to_le_bytes()),
                subrecord(b"DEMO", &5u32.to_le_bytes()),
                subrecord(b"DEVA", &73u32.to_le_bytes()),
            ]),
            1 => fields.extend([
                subrecord(b"PNAM", &package.to_le_bytes()),
                subrecord(b"PNAM", &package.to_le_bytes()),
            ]),
            2 => fields.push(subrecord(b"SNAM", &(index as f32 + 2.375).to_le_bytes())),
            _ => unreachable!("known fixture kind"),
        }
        fields.push(subrecord(b"ANAM", &[]));
    }
    let mut behavior = Vec::new();
    for value in [2u32, 1, 3, 0] {
        behavior.extend(value.to_le_bytes());
    }
    if script_separator {
        fields.push(subrecord(b"NEXT", &[]));
    }
    fields.extend([
        subrecord(b"PNAM", &quest.to_le_bytes()),
        subrecord(b"INAM", &last_action_index.to_le_bytes()),
        subrecord(b"VNAM", &behavior),
        condition(43.25),
    ]);
    named(b"SCEN", id, &fields)
}

/// Build two story-manager quest groups without collapsing their repeated fields.
#[must_use]
pub fn story_quests(id: u32, quest: u32) -> Vec<u8> {
    let mut fields = vec![
        subrecord(b"PNAM", &0u32.to_le_bytes()),
        subrecord(b"SNAM", &0u32.to_le_bytes()),
        subrecord(b"CITC", &1u32.to_le_bytes()),
        condition(3.75),
        subrecord(b"DNAM", &[3, 0, 5, 0]),
        subrecord(b"XNAM", &7u32.to_le_bytes()),
        subrecord(b"MNAM", &2u32.to_le_bytes()),
        subrecord(b"QNAM", &2u32.to_le_bytes()),
    ];
    for (flags, hours) in [(0x1739u32, 3.75f32), (0xa571, 19.125)] {
        fields.extend([
            subrecord(b"NNAM", &quest.to_le_bytes()),
            subrecord(b"FNAM", &flags.to_le_bytes()),
            subrecord(b"RNAM", &hours.to_le_bytes()),
        ]);
    }
    named(b"SMQN", id, &fields)
}

/// Build every location array width and repeated variable world-cell lists.
#[must_use]
pub fn location(id: u32, reference: u32, world: u32) -> Vec<u8> {
    let mut persistent = reference.to_le_bytes().to_vec();
    persistent.extend(world.to_le_bytes());
    persistent.extend((-13i16).to_le_bytes());
    persistent.extend(37i16.to_le_bytes());
    let mut special = 0u32.to_le_bytes().to_vec();
    special.extend(&persistent);
    let mut enable = reference.to_le_bytes().to_vec();
    enable.extend(reference.to_le_bytes());
    enable.extend([3, 0xa5, 0x71, 0x29]);
    let mut fields = Vec::new();
    for tag in [b"ACPR", b"LCPR"] {
        fields.push(subrecord(tag, &persistent));
    }
    fields.push(subrecord(b"RCPR", &reference.to_le_bytes()));
    for tag in [b"ACUN", b"LCUN"] {
        fields.push(subrecord(tag, &[0; 12]));
    }
    fields.push(subrecord(b"RCUN", &0u32.to_le_bytes()));
    for tag in [b"ACSR", b"LCSR"] {
        fields.push(subrecord(tag, &special));
    }
    fields.push(subrecord(b"RCSR", &reference.to_le_bytes()));
    for tag in [b"ACEC", b"LCEC", b"RCEC"] {
        for y in [-17i16, 43] {
            let mut cells = world.to_le_bytes().to_vec();
            for (dy, x) in [(0i16, 29i16), (3, -71)] {
                cells.extend((y + dy).to_le_bytes());
                cells.extend(x.to_le_bytes());
            }
            fields.push(subrecord(tag, &cells));
        }
    }
    for tag in [b"ACID", b"LCID"] {
        fields.push(subrecord(tag, &reference.to_le_bytes()));
    }
    for tag in [b"ACEP", b"LCEP"] {
        fields.push(subrecord(tag, &enable));
    }
    fields.extend([
        subrecord(b"FULL", &text("Synthetic location")),
        subrecord(b"KSIZ", &0u32.to_le_bytes()),
        subrecord(b"KWDA", &[]),
        subrecord(b"PNAM", &0u32.to_le_bytes()),
        subrecord(b"NAM1", &0u32.to_le_bytes()),
        subrecord(b"FNAM", &0u32.to_le_bytes()),
        subrecord(b"MNAM", &reference.to_le_bytes()),
        subrecord(b"RNAM", &127.375f32.to_le_bytes()),
        subrecord(b"NAM0", &reference.to_le_bytes()),
        subrecord(b"CNAM", &[17, 43, 79, 131]),
    ]);
    named(b"LCTN", id, &fields)
}

/// Produce one substantive representative of each assigned native record type.
#[must_use]
pub fn representatives() -> Vec<u8> {
    let branch = named(
        b"DLBR",
        FIRST_ID + 3,
        &[
            subrecord(b"QNAM", &FIRST_ID.to_le_bytes()),
            subrecord(b"TNAM", &1u32.to_le_bytes()),
            subrecord(b"DNAM", &5u32.to_le_bytes()),
            subrecord(b"SNAM", &(FIRST_ID + 1).to_le_bytes()),
        ],
    );
    let topic = named(
        b"DIAL",
        FIRST_ID + 1,
        &[
            subrecord(b"FULL", &text("Synthetic topic")),
            subrecord(b"PNAM", &37.625f32.to_le_bytes()),
            subrecord(b"BNAM", &(FIRST_ID + 3).to_le_bytes()),
            subrecord(b"QNAM", &FIRST_ID.to_le_bytes()),
            subrecord(b"DATA", &[1, 7, 0x66, 0]),
            subrecord(b"SNAM", b"CUST"),
            subrecord(b"TIFC", &1u32.to_le_bytes()),
        ],
    );
    let view = named(
        b"DLVW",
        FIRST_ID + 4,
        &[
            subrecord(b"QNAM", &FIRST_ID.to_le_bytes()),
            subrecord(b"BNAM", &(FIRST_ID + 3).to_le_bytes()),
            subrecord(b"BNAM", &(FIRST_ID + 3).to_le_bytes()),
            subrecord(b"TNAM", &(FIRST_ID + 1).to_le_bytes()),
            subrecord(b"ENAM", &7u32.to_le_bytes()),
            subrecord(b"DNAM", &[1]),
        ],
    );
    let mut story_common = vec![
        subrecord(b"PNAM", &0u32.to_le_bytes()),
        subrecord(b"SNAM", &0u32.to_le_bytes()),
        subrecord(b"CITC", &1u32.to_le_bytes()),
        condition(5.25),
        subrecord(b"DNAM", &3u32.to_le_bytes()),
        subrecord(b"XNAM", &7u32.to_le_bytes()),
    ];
    let story_branch = named(b"SMBN", FIRST_ID + 6, &story_common);
    story_common.push(subrecord(b"ENAM", b"ADDT"));
    let story_event = named(b"SMEN", FIRST_ID + 8, &story_common);
    let message = named(
        b"MESG",
        FIRST_ID + 9,
        &[
            subrecord(b"DESC", &text("Description")),
            subrecord(b"FULL", &text("Synthetic message")),
            subrecord(b"INAM", &0u32.to_le_bytes()),
            subrecord(b"QNAM", &FIRST_ID.to_le_bytes()),
            subrecord(b"DNAM", &3u32.to_le_bytes()),
            subrecord(b"TNAM", &37u32.to_le_bytes()),
            subrecord(b"ITXT", &text("First button")),
            condition(7.25),
            subrecord(b"ITXT", &text("Second button")),
            condition(19.75),
        ],
    );
    [
        quest(FIRST_ID),
        topic,
        response(FIRST_ID + 2, FIRST_ID + 1, 0),
        branch,
        view,
        scene(FIRST_ID + 5, FIRST_ID, FIRST_ID + 1, 0),
        story_branch,
        story_quests(FIRST_ID + 7, FIRST_ID),
        story_event,
        message,
        location(FIRST_ID + 10, 0, 0),
    ]
    .concat()
}
