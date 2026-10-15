//! Original bounded descriptors for successive counted navigation arrays.
//! Layout facts: pinned xEdit TES5 definitions 8015-8236; no external parser code.
use super::FieldSchema;

/// Named dynamic layouts owned by this family.
pub const NAMES: &[&str] = &["world_navmesh", "world_navmesh_info", "world_navmesh_paths"];

/// Construct a primitive descriptor, with offsets relative to its immediate parent.
fn primitive(name: &str, kind: &str, offset: usize) -> FieldSchema {
    FieldSchema {
        name: name.into(),
        kind: kind.into(),
        offset,
        ..Default::default()
    }
}

/// Describe a typed four-byte reference rather than interpreting its high byte.
fn link(name: &str, target: &str, offset: usize) -> FieldSchema {
    FieldSchema {
        targets: vec![target.into()],
        ..primitive(name, "form_id", offset)
    }
}

/// Describe a fixed-width structure whose members preserve all source slots.
fn structure(name: &str, size: usize, offset: usize, members: Vec<FieldSchema>) -> FieldSchema {
    FieldSchema {
        size: Some(size),
        members,
        ..primitive(name, "struct", offset)
    }
}

/// A checked cursor separates binary framing from the generic value interpreter.
struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
    members: Vec<FieldSchema>,
}

impl<'a> Cursor<'a> {
    /// Start at the first byte of one bounded subrecord.
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            position: 0,
            members: Vec::new(),
        }
    }

    /// Advance only after checked addition and source bounds establish the span.
    fn take(&mut self, size: usize) -> Result<usize, String> {
        let start = self.position;
        let end = start
            .checked_add(size)
            .ok_or("navigation extent overflow")?;
        self.bytes
            .get(start..end)
            .ok_or("truncated navigation member")?;
        self.position = end;
        Ok(start)
    }

    /// Retain one scalar while making its source value available for layout decisions.
    fn number(&mut self, name: &str) -> Result<u32, String> {
        let offset = self.take(4)?;
        self.members.push(primitive(name, "u32", offset));
        Ok(u32::from_le_bytes(
            self.bytes[offset..offset + 4]
                .try_into()
                .expect("checked word"),
        ))
    }

    /// Retain a scalar or fixed structure with a known binary width.
    fn member(&mut self, mut member: FieldSchema, size: usize) -> Result<(), String> {
        member.offset = self.take(size)?;
        self.members.push(member);
        Ok(())
    }

    /// A counted fixed-stride array validates multiplication before allocating a descriptor.
    fn array(
        &mut self,
        name: &str,
        stride: usize,
        members: Vec<FieldSchema>,
    ) -> Result<(), String> {
        let count = self.number(&format!("{name}_count"))? as usize;
        let size = count
            .checked_mul(stride)
            .ok_or("navigation array size overflow")?;
        let offset = self.take(size)?;
        self.members.push(FieldSchema {
            size: Some(size),
            count: Some(serde_json::json!({"kind":"remaining","stride":stride})),
            members,
            ..primitive(name, "array", offset)
        });
        Ok(())
    }

    /// Variable-list counts are bounded by their minimum four-byte framing before iteration.
    fn variable_count(&mut self, name: &str) -> Result<usize, String> {
        let count = self.number(name)? as usize;
        if count > 1_000_000 || count > self.bytes.len().saturating_sub(self.position) / 4 {
            return Err("navigation list count exceeds available framing".into());
        }
        Ok(count)
    }

    /// The parent slot is coordinates outdoors and a typed CELL reference indoors.
    fn pathing_cell(&mut self) -> Result<(), String> {
        let start = self.take(12)?;
        let world = u32::from_le_bytes(
            self.bytes[start + 4..start + 8]
                .try_into()
                .expect("checked world"),
        );
        let parent = if world == 0 {
            link("parent_cell", "CELL", 8)
        } else {
            structure(
                "coordinates",
                4,
                8,
                vec![primitive("grid_y", "i16", 0), primitive("grid_x", "i16", 2)],
            )
        };
        self.members.push(structure(
            "pathing_cell",
            12,
            start,
            vec![
                primitive("crc", "u32", 0),
                link("worldspace", "WRLD", 4),
                parent,
            ],
        ));
        Ok(())
    }

    /// Commit the descriptor only when every source byte belongs to a defined member.
    fn finish(self, name: &str) -> Result<FieldSchema, String> {
        if self.position != self.bytes.len() {
            return Err("trailing navigation bytes".into());
        }
        Ok(structure(name, self.position, 0, self.members))
    }
}

/// Three distinct floating-point coordinates describe a source vertex or point.
fn vector() -> Vec<FieldSchema> {
    vec![
        primitive("x", "f32", 0),
        primitive("y", "f32", 4),
        primitive("z", "f32", 8),
    ]
}

/// Six floating-point coordinates describe source minima and maxima without unit conversion.
fn bounds() -> Vec<FieldSchema> {
    ["min_x", "min_y", "min_z", "max_x", "max_y", "max_z"]
        .iter()
        .enumerate()
        .map(|(index, name)| primitive(name, "f32", index * 4))
        .collect()
}

/// NAVM's geometry includes a divisor squared variable-list grid after its fixed arrays.
fn navmesh(bytes: &[u8]) -> Result<FieldSchema, String> {
    let mut cursor = Cursor::new(bytes);
    cursor.number("version")?;
    cursor.pathing_cell()?;
    cursor.array("vertices", 12, vector())?;
    let triangles = vec![
        primitive("vertex_0", "i16", 0),
        primitive("vertex_1", "i16", 2),
        primitive("vertex_2", "i16", 4),
        primitive("edge_0_1", "i16", 6),
        primitive("edge_1_2", "i16", 8),
        primitive("edge_2_0", "i16", 10),
        FieldSchema {
            flags: serde_json::json!({"1":"edge_0_1_link","2":"edge_1_2_link","4":"edge_2_0_link","8":"deleted","16":"no_large_creatures","32":"overlapping","64":"preferred","512":"water","1024":"door","2048":"found"}),
            ..primitive("flags", "u16", 12)
        },
        primitive("cover_flags", "u16", 14),
    ];
    cursor.array("triangles", 16, triangles)?;
    cursor.array("edge_links", 10, vec![FieldSchema { enumeration: serde_json::json!({"0":"portal","1":"ledge_up","2":"ledge_down","3":"enable_disable_portal"}), ..primitive("type", "u32", 0) }, link("navmesh", "NAVM", 4), primitive("triangle", "i16", 8)])?;
    cursor.array(
        "door_triangles",
        10,
        vec![
            primitive("triangle", "i16", 0),
            primitive("crc", "u32", 2),
            link("door", "REFR", 6),
        ],
    )?;
    cursor.array("cover_triangles", 2, vec![primitive("triangle", "i16", 0)])?;
    let divisor = cursor.number("grid_divisor")? as usize;
    cursor.member(primitive("max_x_distance", "f32", 0), 4)?;
    cursor.member(primitive("max_y_distance", "f32", 0), 4)?;
    cursor.member(structure("bounds", 24, 0, bounds()), 24)?;
    let count = divisor
        .checked_mul(divisor)
        .ok_or("navigation grid square overflow")?;
    if count > 1_000_000 || count > bytes.len().saturating_sub(cursor.position) / 4 {
        return Err("navigation grid exceeds bounded available framing".into());
    }
    let start = cursor.position;
    let mut grid = Cursor::new(&bytes[start..]);
    for index in 0..count {
        grid.array(
            &format!("cell_{index}"),
            2,
            vec![primitive("triangle", "i16", 0)],
        )?;
    }
    cursor.position = start
        .checked_add(grid.position)
        .ok_or("navigation grid extent overflow")?;
    cursor
        .members
        .push(structure("grid", grid.position, start, grid.members));
    cursor.finish("geometry")
}

/// NAVI info has conditional island geometry before its final parent-cell structure.
fn info(bytes: &[u8]) -> Result<FieldSchema, String> {
    let mut cursor = Cursor::new(bytes);
    cursor.member(link("navmesh", "NAVM", 0), 4)?;
    cursor.member(
        FieldSchema {
            enumeration: serde_json::json!({"0":"edited","32":"island","64":"not_edited"}),
            ..primitive("category", "u32", 0)
        },
        4,
    )?;
    cursor.member(structure("position", 12, 0, vector()), 12)?;
    cursor.member(
        FieldSchema {
            size: Some(4),
            ..primitive("preferred_merges", "bytes", 0)
        },
        4,
    )?;
    cursor.array("edge_links", 4, vec![link("navmesh", "NAVM", 0)])?;
    cursor.array("preferred_edge_links", 4, vec![link("navmesh", "NAVM", 0)])?;
    cursor.array(
        "door_links",
        8,
        vec![primitive("crc", "u32", 0), link("door", "REFR", 4)],
    )?;
    let island_at = cursor.position;
    cursor.member(primitive("is_island", "u8", 0), 1)?;
    match bytes[island_at] {
        0 => {}
        1 => {
            let start = cursor.position;
            let mut island = Cursor::new(&bytes[start..]);
            island.member(structure("bounds", 24, 0, bounds()), 24)?;
            island.array(
                "triangles",
                6,
                vec![
                    primitive("vertex_0", "i16", 0),
                    primitive("vertex_1", "i16", 2),
                    primitive("vertex_2", "i16", 4),
                ],
            )?;
            island.array("vertices", 12, vector())?;
            cursor.position = start
                .checked_add(island.position)
                .ok_or("navigation island extent overflow")?;
            cursor
                .members
                .push(structure("island", island.position, start, island.members));
        }
        _ => return Err("unsupported navigation island selector".into()),
    }
    cursor.pathing_cell()?;
    cursor.finish("navmesh_info")
}

/// NAVI paths are a counted collection of counted NAVM reference lists and road markers.
fn paths(bytes: &[u8]) -> Result<FieldSchema, String> {
    let mut cursor = Cursor::new(bytes);
    let count = cursor.variable_count("paths_count")?;
    let start = cursor.position;
    let mut paths = Cursor::new(&bytes[start..]);
    for index in 0..count {
        paths.array(
            &format!("path_{index}"),
            4,
            vec![link("navmesh", "NAVM", 0)],
        )?;
    }
    cursor.position = start
        .checked_add(paths.position)
        .ok_or("navigation paths extent overflow")?;
    cursor
        .members
        .push(structure("paths", paths.position, start, paths.members));
    cursor.array(
        "road_markers",
        8,
        vec![link("navmesh", "NAVM", 0), primitive("index", "u32", 4)],
    )?;
    cursor.finish("precomputed_pathing")
}

/// Return an original checked descriptor for an owned name; unrelated selectors pass through.
pub fn layout(name: &str, bytes: &[u8]) -> Result<Option<FieldSchema>, String> {
    match name {
        "world_navmesh" => navmesh(bytes).map(Some),
        "world_navmesh_info" => info(bytes).map(Some),
        "world_navmesh_paths" => paths(bytes).map(Some),
        _ => Ok(None),
    }
}
