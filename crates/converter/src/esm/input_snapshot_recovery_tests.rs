//! A source that changes after metadata admission does not reject usable neighbors.
use super::*;

/// Frame procedural native bytes with the independent 24-byte SE header layout.
fn frame(tag: &[u8; 4], id: u32, fields: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut payload = Vec::new();
    for (signature, bytes) in fields {
        payload.extend(signature);
        payload.extend(u16::try_from(bytes.len()).unwrap().to_le_bytes());
        payload.extend(bytes);
    }
    let mut bytes = tag.to_vec();
    bytes.extend(u32::try_from(payload.len()).unwrap().to_le_bytes());
    bytes.extend(0u32.to_le_bytes());
    bytes.extend(id.to_le_bytes());
    bytes.extend(0u32.to_le_bytes());
    bytes.extend(44u16.to_le_bytes());
    bytes.extend(0u16.to_le_bytes());
    bytes.extend(payload);
    bytes
}

/// Supply one valid header and keyword without using the converter's writer.
fn plugin(path: &Path, id: u32) {
    let mut header = 1.7f32.to_le_bytes().to_vec();
    header.extend(1u32.to_le_bytes());
    header.extend(0x1000u32.to_le_bytes());
    let mut bytes = frame(b"TES4", 0, &[(*b"HEDR", header)]);
    bytes.extend(frame(
        b"KYWD",
        id,
        &[(*b"EDID", b"SnapshotNeighbor\0".to_vec())],
    ));
    std::fs::write(path, bytes).unwrap();
}

/// A late unusable input preserves both surrounding plugins and their admitted slots.
#[test]
fn legacy_snapshot_input_failure_keeps_neighbor_plugins() {
    let directory = tempfile::tempdir().unwrap();
    let paths =
        ["HealthyA.esm", "MutableMod.esp", "HealthyC.esp"].map(|name| directory.path().join(name));
    for (path, id) in paths.iter().zip([0x811, 0x822, 0x933]) {
        plugin(path, id);
    }
    let order = load_order::LoadOrder::read(&paths).unwrap();
    // No live map exists here: truncation between admission and reading is safe on both OSes.
    std::fs::write(&paths[1], b"TES4").unwrap();
    let result = EsmParser::merge_plugins_with_load_order(&paths, &order);
    assert!(
        result.is_ok(),
        "unusable plugin input must not abort neighboring plugins: {result:?}"
    );
    let conversion = result.unwrap();
    assert!(conversion.records.contains_key(&0x0000_0811));
    assert!(conversion.records.contains_key(&0x0200_0933));
    assert!(!conversion.records.contains_key(&0x0100_0822));
    assert_eq!(conversion.warnings.len(), 1);
    assert!(conversion.warnings[0].contains("mutablemod.esp"));
    assert!(conversion.warnings[0].contains("skipped unusable plugin input"));
}
