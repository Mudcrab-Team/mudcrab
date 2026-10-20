//! Synthetic VMAD byte writers, independent of production decoder helpers.

/// Emit the native uint16 byte length followed by the unmodified string bytes.
pub fn text(value: &str) -> Vec<u8> {
    let bytes = value.as_bytes();
    assert!(bytes.len() <= u16::MAX as usize);
    [(bytes.len() as u16).to_le_bytes().as_slice(), bytes].concat()
}

/// Emit one eight-byte object with asymmetric padding and a signed alias sentinel.
pub fn object(format: i16, form_id: u32, alias: i16, unused: u16) -> Vec<u8> {
    match format {
        1 => [
            form_id.to_le_bytes().as_slice(),
            &alias.to_le_bytes(),
            &unused.to_le_bytes(),
        ]
        .concat(),
        2 => [
            unused.to_le_bytes().as_slice(),
            &alias.to_le_bytes(),
            &form_id.to_le_bytes(),
        ]
        .concat(),
        _ => panic!("synthetic object format must be 1 or 2"),
    }
}

/// Emit a property whose value is independently supplied native bytes.
pub fn property(version: i16, name: &str, kind: u8, flags: u8, value: &[u8]) -> Vec<u8> {
    let mut bytes = text(name);
    bytes.push(kind);
    if version >= 4 {
        bytes.push(flags);
    }
    bytes.extend_from_slice(value);
    bytes
}

/// Emit one script with a counted, physically ordered property list.
pub fn script(version: i16, name: &str, flags: u8, properties: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = text(name);
    if version >= 4 {
        bytes.push(flags);
    }
    bytes.extend_from_slice(&(properties.len() as u16).to_le_bytes());
    bytes.extend(properties.concat());
    bytes
}

/// Emit a complete primary header/scripts section and an optional owner-specific tail.
pub fn field(version: i16, format: i16, scripts: &[Vec<u8>], tail: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&version.to_le_bytes());
    bytes.extend_from_slice(&format.to_le_bytes());
    bytes.extend_from_slice(&(scripts.len() as u16).to_le_bytes());
    bytes.extend(scripts.concat());
    bytes.extend_from_slice(tail);
    bytes
}

/// Emit the fragment's signed unknown byte and two nonidentical string names.
pub fn named_fragment(unknown: i8, script: &str, function: &str) -> Vec<u8> {
    let mut bytes = vec![unknown as u8];
    bytes.extend(text(script));
    bytes.extend(text(function));
    bytes
}

/// Emit INFO/PACK/SCEN event headers; the caller supplies bit-ordered fragment bytes.
pub fn events(flags: u8, fragments: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = vec![2, flags];
    bytes.extend(text("DistinctFragmentFile"));
    bytes.extend(fragments.concat());
    bytes
}

/// Emit a quest tail with distinct stage/index, two aliases and differing inner formats.
pub fn quest_tail(outer_format: i16, first_link: u32, second_link: u32) -> Vec<u8> {
    let mut bytes = vec![2];
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend(text("QuestAttachment"));
    bytes.extend_from_slice(&317u16.to_le_bytes());
    bytes.extend_from_slice(&(-1234i16).to_le_bytes());
    bytes.extend_from_slice(&(-87123i32).to_le_bytes());
    bytes.extend(named_fragment(-17, "QuestScript", "StageFunction"));
    bytes.extend_from_slice(&2u16.to_le_bytes());
    for (outer_link, inner_format, alias) in [(first_link, 1i16, -1i16), (second_link, 2, 29)] {
        bytes.extend(object(outer_format, outer_link, alias, 0xA753));
        let properties = vec![property(
            5,
            "AliasObject",
            1,
            1,
            &object(inner_format, outer_link, 37, 0xD149),
        )];
        bytes.extend(field(
            5,
            inner_format,
            &[script(5, "AliasScript", 2, &properties)],
            &[],
        ));
    }
    bytes
}

/// Emit a scene tail whose independent six-byte phase header cannot alias a u32 index.
pub fn scene_tail() -> Vec<u8> {
    let mut bytes = events(2, &[named_fragment(-11, "SceneEndScript", "SceneEnd")]);
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&[2, 173]);
    bytes.extend_from_slice(&(-21917i16).to_le_bytes());
    bytes.push((-29i8) as u8);
    bytes.extend(named_fragment(-37, "PhaseScript", "PhaseCompletion"));
    bytes
}

/// Emit one perk fragment with signed padding and a distinctive index.
pub fn perk_tail() -> Vec<u8> {
    let mut bytes = vec![2];
    bytes.extend(text("PerkAttachment"));
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&751u16.to_le_bytes());
    bytes.extend_from_slice(&(-497i16).to_le_bytes());
    bytes.extend(named_fragment(-41, "PerkScript", "PerkFunction"));
    bytes
}
