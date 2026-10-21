//! Havok 2010.2 binary packfile container, 64-bit layout only.
//!
//! A packfile is a 64-byte header followed by one 48-byte header per section. Every retail
//! Skyrim packfile has exactly three sections (`__classnames__`, `__types__`, `__data__`).
//! Fixups and exports are tables addressed from each section header, not sections of their own.
//! Pointer slots inside `__data__` are written as zero and filled in from those tables.

use color_eyre::{
    Result,
    eyre::{ensure, eyre},
};
use std::collections::HashMap;

pub const MAGIC: [u32; 2] = [0x57E0_E057, 0x10C0_C010];
pub const SUPPORTED_VERSION: &str = "hk_2010.2.0-r1";
pub const FILE_VERSION: u32 = 8;
pub const POINTER_SIZE: u8 = 8;

const HEADER_SIZE: usize = 64;
const SECTION_HEADER_SIZE: usize = 48;
const FIXUP_END: u32 = 0xFFFF_FFFF;
const DATA_SECTION: u32 = 2;

#[derive(Debug, Clone)]
pub struct Section {
    pub name: String,
    pub absolute_start: u32,
    pub local_fixups: u32,
    pub global_fixups: u32,
    pub virtual_fixups: u32,
    pub exports: u32,
    pub imports: u32,
    pub end: u32,
}

impl Section {
    fn bytes<'a>(&self, file: &'a [u8]) -> Result<&'a [u8]> {
        let start = self.absolute_start as usize;
        let end = start + self.end as usize;
        file.get(start..end)
            .ok_or_else(|| eyre!("section {} runs past end of file", self.name))
    }
}

/// An object in `__data__`, located by a virtual fixup.
#[derive(Debug, Clone)]
pub struct Object {
    pub offset: u32,
    pub class: String,
}

#[derive(Debug)]
pub struct Packfile {
    /// Bytes of `__data__`, indexed by section-relative offset.
    pub data: Vec<u8>,
    /// Pointer slot offset -> target offset, both inside `__data__`.
    pub pointers: HashMap<u32, u32>,
    pub objects: Vec<Object>,
    pub contents_offset: u32,
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32> {
    let end = at
        .checked_add(4)
        .ok_or_else(|| eyre!("u32 read at {at:#x} overflows"))?;
    bytes
        .get(at..end)
        .map(|bytes| u32::from_le_bytes(bytes.try_into().expect("four bytes")))
        .ok_or_else(|| eyre!("u32 read at {at:#x} is out of bounds"))
}

fn cstr_at(bytes: &[u8], at: usize) -> Result<String> {
    let tail = bytes
        .get(at..)
        .ok_or_else(|| eyre!("string at {at:#x} is out of bounds"))?;
    let len = tail
        .iter()
        .position(|&byte| byte == 0)
        .ok_or_else(|| eyre!("string at {at:#x} is not NUL-terminated"))?;
    Ok(String::from_utf8_lossy(&tail[..len]).into_owned())
}

impl Packfile {
    pub fn parse(file: &[u8]) -> Result<Self> {
        ensure!(
            file.len() >= HEADER_SIZE,
            "packfile is shorter than its 64-byte header"
        );
        ensure!(
            [u32_at(file, 0)?, u32_at(file, 4)?] == MAGIC,
            "not a Havok packfile: bad magic"
        );

        let version = cstr_at(&file[0x28..0x38], 0)?;
        ensure!(
            version == SUPPORTED_VERSION,
            "unsupported Havok version {version:?}, expected {SUPPORTED_VERSION:?}"
        );

        let file_version = u32_at(file, 0x0C)?;
        ensure!(
            file_version == FILE_VERSION,
            "unsupported packfile version {file_version}, expected {FILE_VERSION}"
        );

        let pointer_size = file[0x10];
        ensure!(
            pointer_size == POINTER_SIZE,
            "unsupported pointer size {pointer_size}, expected {POINTER_SIZE} (Skyrim SE)"
        );

        let little_endian = file[0x11];
        ensure!(little_endian == 1, "big-endian packfiles are not supported");

        let num_sections = u32_at(file, 0x14)?;
        let contents_section = u32_at(file, 0x18)?;
        let contents_offset = u32_at(file, 0x1C)?;
        ensure!(
            contents_section == DATA_SECTION,
            "contents live in section {contents_section}, expected __data__ (2)"
        );

        let mut sections = Vec::with_capacity(num_sections as usize);
        for index in 0..num_sections as usize {
            let at = HEADER_SIZE + index * SECTION_HEADER_SIZE;
            let header = file
                .get(at..at + SECTION_HEADER_SIZE)
                .ok_or_else(|| eyre!("section header {index} is truncated"))?;
            sections.push(Section {
                name: cstr_at(&header[..20], 0)?,
                absolute_start: u32_at(header, 20)?,
                local_fixups: u32_at(header, 24)?,
                global_fixups: u32_at(header, 28)?,
                virtual_fixups: u32_at(header, 32)?,
                exports: u32_at(header, 36)?,
                imports: u32_at(header, 40)?,
                end: u32_at(header, 44)?,
            });
        }

        let names: Vec<&str> = sections
            .iter()
            .map(|section| section.name.as_str())
            .collect();
        ensure!(
            names == ["__classnames__", "__types__", "__data__"],
            "unexpected section layout {names:?}"
        );

        let classnames = sections[0].bytes(file)?;
        let data_section = &sections[DATA_SECTION as usize];
        let raw = data_section.bytes(file)?;

        // The object bytes end where the local fixup table starts.
        let objects_end = data_section.local_fixups as usize;
        let data = raw
            .get(..objects_end)
            .ok_or_else(|| eyre!("__data__ ends before its local fixup table"))?
            .to_vec();

        let local_table = fixup_table(raw, objects_end, data_section.global_fixups, "local")?;
        let global_table = fixup_table(
            raw,
            data_section.global_fixups as usize,
            data_section.virtual_fixups,
            "global",
        )?;
        let virtual_table = fixup_table(
            raw,
            data_section.virtual_fixups as usize,
            data_section.exports,
            "virtual",
        )?;

        let mut pointers = HashMap::new();
        for pair in local_table.as_chunks::<8>().0 {
            let source = u32_at(pair, 0)?;
            if source == FIXUP_END {
                continue;
            }
            pointers.insert(source, u32_at(pair, 4)?);
        }
        for triple in global_table.as_chunks::<12>().0 {
            let source = u32_at(triple, 0)?;
            if source == FIXUP_END {
                continue;
            }
            let target_section = u32_at(triple, 4)?;
            ensure!(
                target_section == DATA_SECTION,
                "global fixup at {source:#x} targets section {target_section}; only __data__ is supported"
            );
            pointers.insert(source, u32_at(triple, 8)?);
        }
        let mut objects = Vec::new();
        for triple in virtual_table.as_chunks::<12>().0 {
            let source = u32_at(triple, 0)?;
            if source == FIXUP_END {
                continue;
            }
            let name_section = u32_at(triple, 4)?;
            ensure!(
                name_section == 0,
                "virtual fixup at {source:#x} names section {name_section}, expected __classnames__"
            );
            objects.push(Object {
                offset: source,
                class: cstr_at(classnames, u32_at(triple, 8)? as usize)?,
            });
        }

        ensure!(
            data_section.exports == data_section.imports
                && data_section.imports == data_section.end,
            "__data__ carries exports/imports; external references are not supported"
        );

        Ok(Packfile {
            data,
            pointers,
            objects,
            contents_offset,
        })
    }

    /// The `__classnames__` table: `(signature, name)` per entry.
    ///
    /// The decoding path resolves names through virtual fixups; this reader exists so the
    /// entry layout (`u32` signature, `0x09` marker, NUL-terminated name, `0xFF` padding) can
    /// be tested directly.
    #[cfg(test)]
    pub fn class_table(file: &[u8], section: &Section) -> Result<Vec<(u32, String)>> {
        let bytes = section.bytes(file)?;
        let mut entries = Vec::new();
        let mut at = 0;
        while at + 5 <= bytes.len() && bytes[at + 4] == 0x09 {
            let name = cstr_at(bytes, at + 5)?;
            let next = at + 5 + name.len() + 1;
            entries.push((u32_at(bytes, at)?, name));
            at = next;
        }
        Ok(entries)
    }

    pub fn object_at(&self, offset: u32) -> Result<&Object> {
        self.objects
            .iter()
            .find(|object| object.offset == offset)
            .ok_or_else(|| eyre!("no object starts at {offset:#x}"))
    }

    pub fn reader(&self, base: u32) -> Reader<'_> {
        Reader {
            packfile: self,
            base,
        }
    }
}

fn fixup_table<'a>(raw: &'a [u8], start: usize, end: u32, name: &str) -> Result<&'a [u8]> {
    let end = end as usize;
    raw.get(start..end)
        .ok_or_else(|| eyre!("__data__ {name} fixup table {start}..{end} is out of bounds"))
}

/// Field reads relative to one object or struct in `__data__`.
#[derive(Clone, Copy)]
pub struct Reader<'a> {
    packfile: &'a Packfile,
    base: u32,
}

impl<'a> Reader<'a> {
    fn absolute(&self, offset: u32) -> Result<u32> {
        self.base
            .checked_add(offset)
            .ok_or_else(|| eyre!("offset {offset:#x} overflows object base {:#x}", self.base))
    }

    fn bytes(&self, offset: u32, len: usize) -> Result<&'a [u8]> {
        let at = self.absolute(offset)? as usize;
        let end = at
            .checked_add(len)
            .ok_or_else(|| eyre!("read at {at:#x} overflows"))?;
        self.packfile
            .data
            .get(at..end)
            .ok_or_else(|| eyre!("read of {len} bytes at {at:#x} past end of __data__"))
    }

    pub fn i32(&self, offset: u32) -> Result<i32> {
        Ok(i32::from_le_bytes(
            self.bytes(offset, 4)?.try_into().expect("four bytes"),
        ))
    }

    pub fn f32(&self, offset: u32) -> Result<f32> {
        Ok(f32::from_le_bytes(
            self.bytes(offset, 4)?.try_into().expect("four bytes"),
        ))
    }

    /// Target of the pointer slot at `offset`, or `None` for a null pointer.
    pub fn ptr(&self, offset: u32) -> Result<Option<u32>> {
        let slot = self.bytes(offset, 8)?;
        let absolute = self.absolute(offset)?;
        ensure!(
            slot.iter().all(|&byte| byte == 0),
            "pointer slot at {absolute:#x} is not zero-filled; fixups would be ignored"
        );
        Ok(self.packfile.pointers.get(&absolute).copied())
    }

    pub fn string(&self, offset: u32) -> Result<Option<String>> {
        match self.ptr(offset)? {
            Some(at) => Ok(Some(cstr_at(&self.packfile.data, at as usize)?)),
            None => Ok(None),
        }
    }

    /// `(data offset, element count)` of the `hkArray` at `offset`.
    pub fn array(&self, offset: u32) -> Result<(u32, u32)> {
        let size_at = offset
            .checked_add(8)
            .ok_or_else(|| eyre!("array size offset overflows"))?;
        let size = u32::from_le_bytes(self.bytes(size_at, 4)?.try_into().expect("four bytes"));
        match self.ptr(offset)? {
            Some(at) => Ok((at, size)),
            None if size == 0 => Ok((0, 0)),
            None => Err(eyre!(
                "hkArray at {:#x} has {size} elements but a null data pointer",
                self.absolute(offset)?
            )),
        }
    }

    pub fn array_bytes(&self, offset: u32, element_size: usize) -> Result<&'a [u8]> {
        let (at, count) = self.array(offset)?;
        let absolute = self.absolute(offset)?;
        let len = (count as usize)
            .checked_mul(element_size)
            .ok_or_else(|| eyre!("array at {absolute:#x} size overflows"))?;
        self.packfile.reader(at).bytes(0, len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLASSNAMES_START: u32 = (HEADER_SIZE + 3 * SECTION_HEADER_SIZE) as u32;

    fn write_u32(bytes: &mut [u8], at: usize, value: u32) {
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn header() -> Vec<u8> {
        let mut bytes = vec![0_u8; HEADER_SIZE];
        write_u32(&mut bytes, 0, MAGIC[0]);
        write_u32(&mut bytes, 4, MAGIC[1]);
        write_u32(&mut bytes, 0x0C, FILE_VERSION);
        bytes[0x10] = POINTER_SIZE;
        bytes[0x11] = 1;
        write_u32(&mut bytes, 0x14, 3);
        write_u32(&mut bytes, 0x18, DATA_SECTION);
        write_u32(&mut bytes, 0x1C, 0);
        bytes[0x28..0x28 + SUPPORTED_VERSION.len()].copy_from_slice(SUPPORTED_VERSION.as_bytes());
        bytes
    }

    fn push_section_header(bytes: &mut Vec<u8>, name: &str, start: u32, offsets: [u32; 6]) {
        let [local, global, virtual_fixups, exports, imports, end] = offsets;
        let mut header = [0_u8; SECTION_HEADER_SIZE];
        header[..name.len()].copy_from_slice(name.as_bytes());
        write_u32(&mut header, 20, start);
        write_u32(&mut header, 24, local);
        write_u32(&mut header, 28, global);
        write_u32(&mut header, 32, virtual_fixups);
        write_u32(&mut header, 36, exports);
        write_u32(&mut header, 40, imports);
        write_u32(&mut header, 44, end);
        bytes.extend_from_slice(&header);
    }

    fn classnames_payload_with(entries: &[(u32, &str)]) -> (Vec<u8>, Vec<u32>) {
        let mut payload = Vec::new();
        let mut name_offsets = Vec::new();
        for &(signature, name) in entries {
            name_offsets.push(payload.len() as u32 + 5);
            payload.extend_from_slice(&signature.to_le_bytes());
            payload.push(0x09);
            payload.extend_from_slice(name.as_bytes());
            payload.push(0);
        }
        payload.extend_from_slice(&[0xFF; 4]);
        (payload, name_offsets)
    }

    fn classnames_payload() -> Vec<u8> {
        classnames_payload_with(&[(0xDEAD_BEEF, "hkRootLevelContainer")]).0
    }

    /// A packfile that passes every guard: one classname entry, empty `__types__`, and an
    /// empty 16-byte `__data__`.
    fn minimal_packfile() -> Vec<u8> {
        let payload = classnames_payload();
        let classnames_len = payload.len() as u32;
        let data_start = CLASSNAMES_START + classnames_len;
        let mut bytes = header();
        push_section_header(
            &mut bytes,
            "__classnames__",
            CLASSNAMES_START,
            [classnames_len; 6],
        );
        push_section_header(&mut bytes, "__types__", data_start, [0; 6]);
        push_section_header(&mut bytes, "__data__", data_start, [16; 6]);
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(&[0_u8; 16]);
        bytes
    }

    fn expect_error(file: &[u8], needle: &str) {
        let error = Packfile::parse(file)
            .expect_err("packfile should have been rejected")
            .to_string();
        assert!(
            error.contains(needle),
            "expected an error containing {needle:?}, got {error:?}"
        );
    }

    #[test]
    fn accepts_a_minimal_retail_shaped_packfile() {
        let packfile = Packfile::parse(&minimal_packfile()).expect("minimal packfile parses");
        assert_eq!(packfile.data.len(), 16);
        assert!(packfile.pointers.is_empty());
        assert!(packfile.objects.is_empty());
        assert_eq!(packfile.contents_offset, 0);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut file = minimal_packfile();
        file[0] = 0;
        expect_error(&file, "bad magic");
    }

    #[test]
    fn rejects_unknown_version() {
        let mut file = minimal_packfile();
        file[0x28..0x28 + SUPPORTED_VERSION.len()].copy_from_slice(b"hk_2014.1.0-r1");
        expect_error(&file, "unsupported Havok version");
    }

    #[test]
    fn rejects_other_file_version() {
        let mut file = minimal_packfile();
        write_u32(&mut file, 0x0C, 11);
        expect_error(&file, "unsupported packfile version");
    }

    #[test]
    fn rejects_32_bit_pointer_size() {
        let mut file = minimal_packfile();
        file[0x10] = 4;
        expect_error(&file, "unsupported pointer size");
    }

    #[test]
    fn rejects_big_endian() {
        let mut file = minimal_packfile();
        file[0x11] = 0;
        expect_error(&file, "big-endian");
    }

    #[test]
    fn rejects_truncated_sections() {
        let file = minimal_packfile();
        expect_error(&file[..HEADER_SIZE], "section header");
    }

    #[test]
    fn rejects_unexpected_section_layout() {
        let mut file = minimal_packfile();
        let types_header = HEADER_SIZE + SECTION_HEADER_SIZE;
        file[types_header..types_header + 9].copy_from_slice(b"__other__");
        expect_error(&file, "unexpected section layout");
    }

    #[test]
    fn rejects_contents_outside_data() {
        let mut file = minimal_packfile();
        write_u32(&mut file, 0x18, 1);
        expect_error(&file, "contents live in section");
    }

    #[test]
    fn rejects_non_zero_pointer_slots() {
        let mut file = minimal_packfile();
        let data_start = CLASSNAMES_START as usize + classnames_payload().len();
        file[data_start] = 1;
        let packfile = Packfile::parse(&file).expect("guard passes; the slot is read later");
        let error = packfile
            .reader(0)
            .ptr(0)
            .expect_err("non-zero pointer slot is rejected")
            .to_string();
        assert!(error.contains("not zero-filled"), "got {error:?}");
    }

    #[test]
    fn rejects_exports() {
        let mut file = minimal_packfile();
        let data_header = HEADER_SIZE + 2 * SECTION_HEADER_SIZE;
        // Keep the three fixup tables empty so the exports check is the first failure.
        write_u32(&mut file, data_header + 24, 0);
        write_u32(&mut file, data_header + 28, 0);
        write_u32(&mut file, data_header + 32, 0);
        write_u32(&mut file, data_header + 36, 8);
        expect_error(&file, "external references are not supported");
    }

    #[test]
    fn class_table_reads_signature_marker_and_name() {
        let (payload, _) = classnames_payload_with(&[
            (0xDEAD_BEEF, "hkRootLevelContainer"),
            (0x1234_5678, "hkaAnimationContainer"),
        ]);
        let classnames_len = payload.len() as u32;
        let data_start = CLASSNAMES_START + classnames_len;
        let mut file = header();
        push_section_header(
            &mut file,
            "__classnames__",
            CLASSNAMES_START,
            [classnames_len; 6],
        );
        push_section_header(&mut file, "__types__", data_start, [0; 6]);
        push_section_header(&mut file, "__data__", data_start, [16; 6]);
        file.extend_from_slice(&payload);
        file.extend_from_slice(&[0_u8; 16]);

        let section = Section {
            name: "__classnames__".to_owned(),
            absolute_start: CLASSNAMES_START,
            local_fixups: classnames_len,
            global_fixups: classnames_len,
            virtual_fixups: classnames_len,
            exports: classnames_len,
            imports: classnames_len,
            end: classnames_len,
        };
        let table = Packfile::class_table(&file, &section).expect("class table parses");
        assert_eq!(
            table,
            vec![
                (0xDEAD_BEEF, "hkRootLevelContainer".to_owned()),
                (0x1234_5678, "hkaAnimationContainer".to_owned()),
            ]
        );
    }

    #[test]
    fn fixup_tables_resolve_local_global_and_virtual_entries() {
        let (payload, name_offsets) = classnames_payload_with(&[
            (0x1111_1111, "hkRootLevelContainer"),
            (0x2222_2222, "hkaAnimationContainer"),
        ]);
        let classnames_len = payload.len() as u32;
        let data_start = CLASSNAMES_START + classnames_len;
        let object_bytes = 16_u32;
        let table_len = 24_u32;
        let global_start = object_bytes + table_len;
        let virtual_start = global_start + table_len;
        let exports_start = virtual_start + table_len;

        let mut file = header();
        push_section_header(
            &mut file,
            "__classnames__",
            CLASSNAMES_START,
            [classnames_len; 6],
        );
        push_section_header(&mut file, "__types__", data_start, [0; 6]);
        push_section_header(
            &mut file,
            "__data__",
            data_start,
            [
                object_bytes,
                global_start,
                virtual_start,
                exports_start,
                exports_start,
                exports_start,
            ],
        );
        file.extend_from_slice(&payload);
        file.extend_from_slice(&[0_u8; 16]);
        for (source, target) in [(0x10_u32, 0x20_u32), (0x30, 0x40)] {
            file.extend_from_slice(&source.to_le_bytes());
            file.extend_from_slice(&target.to_le_bytes());
        }
        file.extend_from_slice(&FIXUP_END.to_le_bytes());
        file.extend_from_slice(&FIXUP_END.to_le_bytes());
        let (source, target) = (0x08_u32, 0x28_u32);
        file.extend_from_slice(&source.to_le_bytes());
        file.extend_from_slice(&DATA_SECTION.to_le_bytes());
        file.extend_from_slice(&target.to_le_bytes());
        file.extend_from_slice(&FIXUP_END.to_le_bytes());
        file.extend_from_slice(&0_u32.to_le_bytes());
        file.extend_from_slice(&0_u32.to_le_bytes());
        file.extend_from_slice(&0_u32.to_le_bytes());
        file.extend_from_slice(&0_u32.to_le_bytes());
        file.extend_from_slice(&name_offsets[0].to_le_bytes());
        file.extend_from_slice(&FIXUP_END.to_le_bytes());
        file.extend_from_slice(&0_u32.to_le_bytes());
        file.extend_from_slice(&0_u32.to_le_bytes());

        let packfile = Packfile::parse(&file).expect("fixup tables parse");
        assert_eq!(packfile.pointers.get(&0x10), Some(&0x20));
        assert_eq!(packfile.pointers.get(&0x30), Some(&0x40));
        assert_eq!(packfile.pointers.get(&0x08), Some(&0x28));
        assert!(!packfile.pointers.contains_key(&FIXUP_END));
        assert_eq!(packfile.objects.len(), 1);
        assert_eq!(packfile.objects[0].offset, 0);
        assert_eq!(packfile.objects[0].class, "hkRootLevelContainer");
        assert_eq!(packfile.data.len(), object_bytes as usize);
    }
}
