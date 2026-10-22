//! BSA name decoding and the TES4 name hash.
//!
//! BSA names are Windows-1252 bytes (not UTF-8), and every folder and file
//! record stores a 64-bit hash of its lowercase name that the game uses for
//! lookup. The hash is computed over the stored bytes, so a name that decodes
//! the same but hashes differently is an archive the game cannot read.

/// Windows-1252 characters for bytes 0x80..=0x9f. The five bytes Windows leaves
/// undefined map to the C1 control of the same value, as Windows does.
const CP1252_HIGH: [char; 32] = [
    '\u{20ac}', '\u{81}', '\u{201a}', '\u{192}', '\u{201e}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{2c6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}', '\u{8d}', '\u{17d}', '\u{8f}',
    '\u{90}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{2dc}', '\u{2122}', '\u{161}', '\u{203a}', '\u{153}', '\u{9d}', '\u{17e}', '\u{178}',
];

/// Decodes a stored BSA name. Lossless: every byte maps to one character.
pub(super) fn decode_name(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&byte| match byte {
            0x80..=0x9f => CP1252_HIGH[usize::from(byte - 0x80)],
            _ => char::from(byte),
        })
        .collect()
}

/// Hash of a file name (`name.ext`, no folder part).
pub(super) fn file_hash(name: &[u8]) -> u64 {
    let lowered = lower(name, false);
    match lowered.iter().rposition(|&byte| byte == b'.') {
        Some(dot) => hash(&lowered[..dot], &lowered[dot..]),
        None => hash(&lowered, &[]),
    }
}

/// Hash of a folder path. Slash direction is ignored; the hash always uses
/// backslashes, as the game does.
pub(super) fn folder_hash(path: &[u8]) -> u64 {
    hash(&lower(path, true), &[])
}

fn lower(name: &[u8], backslashes: bool) -> Vec<u8> {
    name.iter()
        .map(|&byte| match byte {
            b'/' if backslashes => b'\\',
            _ => byte.to_ascii_lowercase(),
        })
        .collect()
}

fn hash(root: &[u8], extension: &[u8]) -> u64 {
    let mut low = 0u32;
    let mut middle = 0u32;
    if let (Some(&first), Some(&last)) = (root.first(), root.last()) {
        let second_last = if root.len() > 2 {
            root[root.len() - 2]
        } else {
            0
        };
        // The length byte holds at most 255, as the stored name length does.
        low = u32::from(last)
            | u32::from(second_last) << 8
            | (root.len() as u32 & 0xff) << 16
            | u32::from(first) << 24;
        if root.len() > 3 {
            middle = crc(&root[1..root.len() - 2]);
        }
    }
    low |= match extension {
        b".kf" => 0x80,
        b".nif" => 0x8000,
        b".dds" => 0x8080,
        b".wav" => 0x8000_0000,
        b".adp" => 0x8000_8000,
        _ => 0,
    };
    let high = middle.wrapping_add(crc(extension));
    u64::from(high) << 32 | u64::from(low)
}

fn crc(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0u32, |crc, &byte| {
        crc.wrapping_mul(0x1003f).wrapping_add(u32::from(byte))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_windows_1252_names_losslessly() {
        assert_eq!(
            decode_name(b"caf\xe9/na\xefve\x80.nif"),
            "caf\u{e9}/na\u{ef}ve\u{20ac}.nif"
        );
        assert_eq!(decode_name(b"plain/ascii.dds"), "plain/ascii.dds");
        assert!(!decode_name(&[0x92, 0xe9, 0xff]).contains('\u{fffd}'));
    }

    #[test]
    fn hashes_ignore_case_and_slash_direction() {
        assert_eq!(
            folder_hash(b"Meshes/Actors"),
            folder_hash(b"meshes\\actors")
        );
        assert_eq!(file_hash(b"Dragon.NIF"), file_hash(b"dragon.nif"));
        assert_ne!(file_hash(b"dragon.nif"), file_hash(b"dragon.dds"));
    }

    /// Pairs read from the stored hashes in the vanilla `Skyrim - Interface.bsa`,
    /// so a wrong algorithm cannot pass by agreeing with itself.
    #[test]
    fn matches_hashes_stored_in_a_vanilla_archive() {
        assert_eq!(
            folder_hash(b"interface\x5ccontrols\x5c360"),
            0x3dc5_f3f9_6916_3630
        );
        assert_eq!(
            folder_hash(b"interface/controls/durango"),
            0x443a_f134_691a_676f
        );
        assert_eq!(file_hash(b"keyboard_english.txt"), 0x1fdc_44b0_6b10_7368);
        assert_eq!(file_hash(b"gamepad.txt"), 0x6cf7_7259_6707_6164);
        assert_eq!(file_hash(b"controlmap.txt"), 0xafb8_af35_630a_6170);
    }

    /// The `.adp` extension adds 0x8000_8000 (extension index 5 sets the top bit of the first
    /// byte and bit 15), worked out by hand from the published extension-index transform. The
    /// low word of `voice.adp` is the stem word for `voice` (0x7605_6365) with that added.
    #[test]
    fn adp_files_use_the_extension_index_bits() {
        assert_eq!(file_hash(b"voice.adp") as u32, 0xf605_e365);
        assert_eq!(file_hash(b"voice.wav") as u32, 0xf605_6365);
        assert_eq!(file_hash(b"voice.nif") as u32, 0x7605_e365);
    }

    #[test]
    fn handles_names_shorter_than_the_hash_window() {
        for name in [&b""[..], b"a", b"ab", b"abc", b"abcd", b".nif", b"x."] {
            let _ = file_hash(name);
            let _ = folder_hash(name);
        }
    }
}
