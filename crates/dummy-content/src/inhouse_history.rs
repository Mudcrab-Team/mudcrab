//! Independent native fixtures for accepted, deleted and rejected override versions.
use crate::{esm, layout};
use color_eyre::Result;
use std::{fs, path::Path};

/// Frame native subrecords without using converter schemas or serialization.
fn subrecord(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    let mut out = tag.to_vec();
    out.extend((bytes.len() as u16).to_le_bytes());
    out.extend(bytes);
    out
}

/// Frame an SSE record with independent header sentinels.
fn record(tag: &[u8; 4], id: u32, flags: u32, bytes: &[u8]) -> Vec<u8> {
    let mut out = tag.to_vec();
    out.extend((bytes.len() as u32).to_le_bytes());
    out.extend(flags.to_le_bytes());
    out.extend(id.to_le_bytes());
    out.extend(0x1357_2468u32.to_le_bytes());
    out.extend(44u16.to_le_bytes());
    out.extend(0xA571u16.to_le_bytes());
    out.extend(bytes);
    out
}

/// Give otherwise identical versions different editor names.
fn stat(id: u32, name: &str) -> Vec<u8> {
    record(
        b"STAT",
        id,
        0,
        &subrecord(b"EDID", format!("{name}\0").as_bytes()),
    )
}

/// EditorID aliases deliberately use different native FormIDs and letter casing.
fn setting(id: u32, name: &str, value: &[u8]) -> Vec<u8> {
    record(
        b"GMST",
        id,
        0,
        &[
            subrecord(b"EDID", format!("{name}\0").as_bytes()),
            subrecord(b"DATA", value),
        ]
        .concat(),
    )
}

/// Declare master lists independently of load-order slot numbering.
fn header(masters: &[&str], flags: u32) -> Vec<u8> {
    let mut body = subrecord(
        b"HEDR",
        &[
            1.7f32.to_le_bytes(),
            20u32.to_le_bytes(),
            0x800u32.to_le_bytes(),
        ]
        .concat(),
    );
    for master in masters {
        body.extend(subrecord(b"MAST", format!("{master}\0").as_bytes()));
        body.extend(subrecord(b"DATA", &[0xA5; 8]));
    }
    record(b"TES4", 0, flags, &body)
}

/// Generate assets and five asymmetric plugins, returning their explicit load-order list.
pub fn generate(data: &Path) -> Result<Vec<String>> {
    layout::prepare_directory(data, false)?;
    layout::generate(
        data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,nif,pex,esm")?,
    )?;
    let mut base = esm::plugin(&esm::Plugin {
        author: layout::GENERATED_AUTHOR,
        worldspace: layout::GENERATED_WORLDSPACE,
        cells: &[esm::PRESET_EXTERIOR_CELL],
        model_path: layout::GENERATED_MODEL_PATH,
        diffuse: layout::GENERATED_DIFFUSE_PATH,
        normal_texture: layout::GENERATED_NORMAL_PATH,
    })?;
    for id in [0x1800, 0x1801, 0x1802, 0x1803] {
        base.extend(stat(id, "Original"));
    }
    base.extend(setting(0x1810, "fHistoryAlias", &11.25f32.to_le_bytes()));
    base.extend(setting(0x1820, "fBrokenHistory", &17.5f32.to_le_bytes()));
    fs::write(data.join("Skyrim.esm"), base)?;
    fs::write(data.join("Spacer.esp"), header(&["Skyrim.esm"], 0))?;
    let mut patch = header(&["Skyrim.esm"], 0);
    patch.extend(stat(0x1800, "Second"));
    patch.extend(record(b"STAT", 0x1801, 0x20, &[]));
    patch.extend(record(b"STAT", 0x1803, 0x40000, &[4, 0, 0, 0, 0xFF]));
    patch.extend(setting(
        0x0100_1811,
        "FHISTORYALIAS",
        &(-42.5f32).to_le_bytes(),
    ));
    // New native alias plus valid EDID, then unusable mandatory DATA.
    patch.extend(setting(0x0100_182F, "FBROKENHISTORY", &[0xA7]));
    // The EDID framing itself is broken, so history must retain the raw alias.
    let mut broken_editor = b"EDID".to_vec();
    broken_editor.extend(99u16.to_le_bytes());
    broken_editor.extend(b"fBrokenHistory\0");
    patch.extend(record(b"GMST", 0x0100_1811, 0, &broken_editor));
    patch.extend(stat(0x1860, "Injected"));
    patch.extend(stat(0x0100_1870, "UsableNeighbor"));
    fs::write(data.join("Patch.esp"), patch)?;
    let mut light = header(&["Skyrim.esm", "Patch.esp"], 0x200);
    light.extend(stat(0x1800, "ThirdLight"));
    light.extend(stat(0x1801, "RestoredLight"));
    light.extend(setting(
        0x0200_0A40,
        "fHistoryAlias",
        &173.5f32.to_le_bytes(),
    ));
    light.extend(stat(0x0200_0A42, "LightOriginal"));
    fs::write(data.join("Light.esp"), light)?;
    let mut last = header(&["Skyrim.esm", "Patch.esp", "Light.esp"], 0);
    last.extend(stat(0x1800, "Fourth"));
    last.extend(record(b"STAT", 0x1802, 0x20, &[]));
    last.extend(stat(0x0200_0A42, "LightOverriddenByFull"));
    last.extend(setting(
        0x0300_1812,
        "fHistoryAlias",
        &99.75f32.to_le_bytes(),
    ));
    last.extend(stat(0x1860, "InjectedOverride"));
    fs::write(data.join("Last.esp"), last)?;
    let names = [
        "Skyrim.esm",
        "Spacer.esp",
        "Patch.esp",
        "Light.esp",
        "Last.esp",
    ]
    .map(str::to_owned)
    .to_vec();
    fs::write(
        data.join("plugins.txt"),
        names
            .iter()
            .map(|name| format!("*{name}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )?;
    Ok(names)
}
