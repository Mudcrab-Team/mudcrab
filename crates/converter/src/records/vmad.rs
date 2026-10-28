//! Independently authored, bounded VMAD layouts for Skyrim script attachments.
//!
//! Format facts: pinned xEdit TES5 definitions 4682–4956 and UTF-8 selection
//! in wbInterface.pas23752. Canonical bytes preserve padding and unknown numeric
//! fields; only explicitly decoded object FormIDs are rewritten.
use super::Value;

/// Decode a complete VMAD field, resolving every primary and alias object link.
/// Errors describe this field only and must be handled by the caller's field recovery.
pub fn decode(
    bytes: &[u8],
    owner: &[u8; 4],
    remap: impl FnMut(u32) -> Result<u32, String>,
) -> Result<(Value, Vec<u8>), String> {
    let mut input = Cursor {
        source: bytes,
        canonical: bytes.to_vec(),
        offset: 0,
        remap,
    };
    let version = input.i16()?;
    let object_format = input.i16()?;
    check_header(version, object_format)?;
    let count = input.u16()?;
    let scripts = input.scripts(count, version, object_format)?;
    let mut members = vec![
        ("version".into(), Value::Signed(i64::from(version))),
        (
            "object_format".into(),
            Value::Signed(i64::from(object_format)),
        ),
        ("script_count".into(), Value::Unsigned(u64::from(count))),
        ("scripts".into(), Value::Array(scripts)),
    ];
    if input.offset != bytes.len() {
        members.push(("fragments".into(), input.fragments(owner, object_format)?));
    }
    if input.offset != bytes.len() {
        return Err(format!(
            "VMAD has {} unaccounted trailing bytes for {}",
            bytes.len() - input.offset,
            String::from_utf8_lossy(owner)
        ));
    }
    Ok((Value::Struct(members), input.canonical))
}

/// Refuse unsupported headers without guessing a different object or script layout.
fn check_header(version: i16, format: i16) -> Result<(), String> {
    if !(1..=5).contains(&version) {
        return Err(format!("unsupported VMAD version {version}"));
    }
    if !matches!(format, 1 | 2) {
        return Err(format!("unsupported VMAD object format {format}"));
    }
    Ok(())
}

/// Sequential variable-width reads keep all allocations bounded by the actual field.
struct Cursor<'a, F> {
    source: &'a [u8],
    canonical: Vec<u8>,
    offset: usize,
    remap: F,
}

impl<F: FnMut(u32) -> Result<u32, String>> Cursor<'_, F> {
    /// Consume exactly one checked span, never advancing on a failed boundary.
    fn take(&mut self, size: usize) -> Result<&[u8], String> {
        let end = self.offset.checked_add(size).ok_or("VMAD size overflow")?;
        let bytes = self
            .source
            .get(self.offset..end)
            .ok_or_else(|| format!("VMAD truncated at {}: needs {size} bytes", self.offset))?;
        self.offset = end;
        Ok(bytes)
    }

    /// Read one unsigned byte.
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    /// Read one signed byte, preserving source-known unknown fragment values.
    fn i8(&mut self) -> Result<i8, String> {
        Ok(self.u8()? as i8)
    }

    /// Read one unsigned little-endian word.
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().expect("checked word"),
        ))
    }

    /// Read one signed little-endian word.
    fn i16(&mut self) -> Result<i16, String> {
        Ok(self.u16()? as i16)
    }

    /// Read one unsigned little-endian double word.
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("checked double word"),
        ))
    }

    /// Read one signed little-endian double word.
    fn i32(&mut self) -> Result<i32, String> {
        Ok(self.u32()? as i32)
    }

    /// Read an exact float32 value; nonfinite values reject only this VMAD field.
    fn float(&mut self) -> Result<Value, String> {
        let value = f32::from_bits(self.u32()?);
        if !value.is_finite() {
            return Err("VMAD nonfinite float".into());
        }
        Ok(Value::Float(value))
    }

    /// Consume a uint16-length UTF-8 string, the pinned native VMAD encoding.
    fn text(&mut self) -> Result<Value, String> {
        let size = usize::from(self.u16()?);
        let bytes = self.take(size)?;
        let text = std::str::from_utf8(bytes).map_err(|_| "VMAD invalid UTF-8 string")?;
        Ok(Value::String(text.to_owned()))
    }

    /// Reject impossible counts before reserving or iterating attacker-selected entries.
    fn bounded(&self, count: u32, minimum: usize) -> Result<usize, String> {
        let count = usize::try_from(count).map_err(|_| "VMAD count exceeds address space")?;
        if count > (self.source.len() - self.offset) / minimum {
            return Err(format!("VMAD count {count} exceeds remaining bytes"));
        }
        Ok(count)
    }

    /// Interpret the two object formats while retaining their signed alias and padding.
    fn object(&mut self, format: i16) -> Result<Value, String> {
        let start = self.offset;
        let raw: [u8; 8] = self.take(8)?.try_into().expect("checked object");
        let (link_offset, alias_offset, unused_offset) = match format {
            1 => (0, 4, 6),
            2 => (4, 2, 0),
            _ => return Err(format!("unsupported VMAD object format {format}")),
        };
        let source_id = u32::from_le_bytes(raw[link_offset..link_offset + 4].try_into().unwrap());
        let form_id = (self.remap)(source_id)?;
        self.canonical[start + link_offset..start + link_offset + 4]
            .copy_from_slice(&form_id.to_le_bytes());
        let alias = i16::from_le_bytes(raw[alias_offset..alias_offset + 2].try_into().unwrap());
        let unused = u16::from_le_bytes(raw[unused_offset..unused_offset + 2].try_into().unwrap());
        Ok(Value::Struct(vec![
            ("form_id".into(), Value::FormId(form_id)),
            ("alias".into(), Value::Signed(i64::from(alias))),
            ("unused".into(), Value::Unsigned(u64::from(unused))),
        ]))
    }

    /// Decode all scalar property types, with zero denoting an explicit absent value.
    fn scalar(&mut self, kind: u8, format: i16) -> Result<Value, String> {
        match kind {
            0 => Ok(Value::Struct(Vec::new())),
            1 => self.object(format),
            2 => self.text(),
            3 => Ok(Value::Signed(i64::from(self.i32()?))),
            4 => self.float(),
            5 => Ok(Value::Unsigned(u64::from(self.u8()?))),
            _ => Err(format!("unsupported VMAD property type {kind}")),
        }
    }

    /// Decode a property and its optional flags without discarding empty arrays.
    fn property(&mut self, version: i16, format: i16) -> Result<Value, String> {
        let name = self.text()?;
        let kind = self.u8()?;
        let mut members = vec![
            ("name".into(), name),
            ("type".into(), Value::Unsigned(u64::from(kind))),
        ];
        if version >= 4 {
            members.push(("flags".into(), Value::Unsigned(u64::from(self.u8()?))));
        }
        let value = if (11..=15).contains(&kind) {
            let count = self.u32()?;
            let scalar_kind = kind - 10;
            let minimum = match scalar_kind {
                1 => 8,
                2 => 2,
                3 | 4 => 4,
                _ => 1,
            };
            let count_usize = self.bounded(count, minimum)?;
            let mut elements = Vec::with_capacity(count_usize);
            for _ in 0..count_usize {
                elements.push(self.scalar(scalar_kind, format)?);
            }
            members.push(("array_count".into(), Value::Unsigned(u64::from(count))));
            Value::Array(elements)
        } else {
            self.scalar(kind, format)?
        };
        members.push(("value".into(), value));
        Ok(Value::Struct(members))
    }

    /// Decode one script while retaining physical property order and duplicate names.
    fn script(&mut self, version: i16, format: i16) -> Result<Value, String> {
        let mut members = vec![("name".into(), self.text()?)];
        if version >= 4 {
            members.push(("flags".into(), Value::Unsigned(u64::from(self.u8()?))));
        }
        let count = self.u16()?;
        let count_usize = self.bounded(u32::from(count), if version >= 4 { 4 } else { 3 })?;
        let mut properties = Vec::with_capacity(count_usize);
        for _ in 0..count_usize {
            properties.push(self.property(version, format)?);
        }
        members.push(("property_count".into(), Value::Unsigned(u64::from(count))));
        members.push(("properties".into(), Value::Array(properties)));
        Ok(Value::Struct(members))
    }

    /// Decode a counted script list with minimum sizes appropriate to its own header.
    fn scripts(&mut self, count: u16, version: i16, format: i16) -> Result<Vec<Value>, String> {
        let count = self.bounded(u32::from(count), if version >= 4 { 5 } else { 4 })?;
        let mut scripts = Vec::with_capacity(count);
        for _ in 0..count {
            scripts.push(self.script(version, format)?);
        }
        Ok(scripts)
    }

    /// Decode a named fragment's shared unknown byte and pair of length strings.
    fn named_fragment(&mut self) -> Result<Vec<(String, Value)>, String> {
        Ok(vec![
            ("unknown".into(), Value::Signed(i64::from(self.i8()?))),
            ("script_name".into(), self.text()?),
            ("fragment_name".into(), self.text()?),
        ])
    }

    /// Every set bit frames one named fragment; unknown event meanings retain their bit.
    fn events(&mut self, flags: u8) -> Result<Value, String> {
        let mut entries = Vec::with_capacity(flags.count_ones() as usize);
        for index in 0..8 {
            let bit = 1u8 << index;
            if flags & bit != 0 {
                let mut members = vec![("event".into(), Value::Unsigned(u64::from(bit)))];
                members.extend(self.named_fragment()?);
                entries.push(Value::Struct(members));
            }
        }
        Ok(Value::Array(entries))
    }

    /// Decode quest aliases using the outer object format and each alias's own script header.
    fn aliases(&mut self, outer_format: i16) -> Result<Value, String> {
        let count = self.u16()?;
        let count_usize = self.bounded(u32::from(count), 14)?;
        let mut aliases = Vec::with_capacity(count_usize);
        for _ in 0..count_usize {
            let object = self.object(outer_format)?;
            let version = self.i16()?;
            let format = self.i16()?;
            check_header(version, format)?;
            let script_count = self.u16()?;
            let scripts = self.scripts(script_count, version, format)?;
            aliases.push(Value::Struct(vec![
                ("object".into(), object),
                ("version".into(), Value::Signed(i64::from(version))),
                ("object_format".into(), Value::Signed(i64::from(format))),
                (
                    "script_count".into(),
                    Value::Unsigned(u64::from(script_count)),
                ),
                ("scripts".into(), Value::Array(scripts)),
            ]));
        }
        Ok(Value::Struct(vec![
            ("count".into(), Value::Unsigned(u64::from(count))),
            ("entries".into(), Value::Array(aliases)),
        ]))
    }

    /// Decode the five owner-specific fragment layouts with all named unknown fields.
    fn fragments(&mut self, owner: &[u8; 4], object_format: i16) -> Result<Value, String> {
        if !matches!(owner, b"INFO" | b"PACK" | b"PERK" | b"QUST" | b"SCEN") {
            return Err(format!(
                "VMAD fragment tail is invalid for {}",
                String::from_utf8_lossy(owner)
            ));
        }
        let mut members = vec![("bind_version".into(), Value::Signed(i64::from(self.i8()?)))];
        match owner {
            b"INFO" | b"PACK" | b"SCEN" => {
                let flags = self.u8()?;
                members.push(("flags".into(), Value::Unsigned(u64::from(flags))));
                members.push(("file_name".into(), self.text()?));
                members.push(("events".into(), self.events(flags)?));
                if owner == b"SCEN" {
                    let count = self.u16()?;
                    let count_usize = self.bounded(u32::from(count), 10)?;
                    let mut phases = Vec::with_capacity(count_usize);
                    for _ in 0..count_usize {
                        let mut phase = vec![
                            ("flags".into(), Value::Unsigned(u64::from(self.u8()?))),
                            ("phase_index".into(), Value::Unsigned(u64::from(self.u8()?))),
                            ("unknown_word".into(), Value::Signed(i64::from(self.i16()?))),
                            ("unknown_byte".into(), Value::Signed(i64::from(self.i8()?))),
                        ];
                        phase.extend(self.named_fragment()?);
                        phases.push(Value::Struct(phase));
                    }
                    members.push(("phase_count".into(), Value::Unsigned(u64::from(count))));
                    members.push(("phases".into(), Value::Array(phases)));
                }
            }
            b"PERK" => {
                members.push(("file_name".into(), self.text()?));
                let count = self.u16()?;
                let count_usize = self.bounded(u32::from(count), 9)?;
                let mut fragments = Vec::with_capacity(count_usize);
                for _ in 0..count_usize {
                    let mut fragment = vec![
                        ("index".into(), Value::Unsigned(u64::from(self.u16()?))),
                        ("unknown_word".into(), Value::Signed(i64::from(self.i16()?))),
                    ];
                    fragment.extend(self.named_fragment()?);
                    fragments.push(Value::Struct(fragment));
                }
                members.push(("count".into(), Value::Unsigned(u64::from(count))));
                members.push(("entries".into(), Value::Array(fragments)));
            }
            b"QUST" => {
                let count = self.u16()?;
                members.push(("count".into(), Value::Unsigned(u64::from(count))));
                members.push(("file_name".into(), self.text()?));
                let count_usize = self.bounded(u32::from(count), 13)?;
                let mut fragments = Vec::with_capacity(count_usize);
                for _ in 0..count_usize {
                    let mut fragment = vec![
                        ("stage".into(), Value::Unsigned(u64::from(self.u16()?))),
                        ("unknown_word".into(), Value::Signed(i64::from(self.i16()?))),
                        ("stage_index".into(), Value::Signed(i64::from(self.i32()?))),
                    ];
                    fragment.extend(self.named_fragment()?);
                    fragments.push(Value::Struct(fragment));
                }
                members.push(("entries".into(), Value::Array(fragments)));
                members.push(("aliases".into(), self.aliases(object_format)?));
            }
            _ => unreachable!("checked fragment owner"),
        }
        Ok(Value::Struct(members))
    }
}
