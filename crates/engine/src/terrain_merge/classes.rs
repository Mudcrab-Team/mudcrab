//! The land-texture classes the merged terrain material binds (#215).
//!
//! The converter writes one `texture_2d_array` file per class (usage, block format, size) and
//! lists only the classes the content uses ([`shared::terrain_mesh::LandTextureTable`]). The
//! material binds those classes as static array bindings, one slot per listed class, and the
//! shader is specialised to exactly the listed slots, so a vanilla table (4 diffuse and 2 normal
//! classes) compiles a shader that knows only those six.
//!
//! Slot assignment is dense and in table order, per usage. The sampler of the whole material is
//! tied to diffuse slot 0, so a table must list at least one diffuse class; every entry names a
//! diffuse layer, so a valid non-empty table always does.
//!
//! The group-4 binding-array path is the later step: [`TerrainClasses`] keeps the class list and
//! the packed lookup words independent of how the arrays reach the shader, so only the
//! material's bindings and the sampling helpers in `terrain_merged.wgsl` change.

use shared::terrain_mesh::{
    ArrayLayer, DIFFUSE_FORMATS, LAND_TEXTURE_SIZE_CLASSES, LandTextureArray, LandTextureFormat,
    LandTextureTable, LandTextureUsage, NORMAL_FORMATS, TerrainMeshError,
};
use std::collections::HashMap;

/// The most classes one usage can have: every allowed block format in every size class. A valid
/// table never exceeds it, so the material's fixed slot count is the contract's own limit and
/// nothing is capped below it.
pub const MAX_CLASSES_PER_USAGE: usize = DIFFUSE_FORMATS.len() * LAND_TEXTURE_SIZE_CLASSES.len();

const _: () = assert!(
    NORMAL_FORMATS.len() * LAND_TEXTURE_SIZE_CLASSES.len() == MAX_CLASSES_PER_USAGE,
    "diffuse and normal usages must have the same number of classes"
);

/// The packed lookup word of an entry with no normal map.
pub const NO_NORMAL_WORD: u32 = u32::MAX;

/// Sampled textures the material adds to every pipeline that uses it: every diffuse slot and
/// every normal slot, bound or not, because a slot is in the bind group layout either way.
pub const TERRAIN_CLASS_BINDINGS: u32 = (2 * MAX_CLASSES_PER_USAGE) as u32;

/// What the checks leave for the rest of Bevy's PBR pipeline (the standard material's textures,
/// shadow maps, environment maps, prepass textures): a deliberately generous allowance, since the
/// real count depends on the features a view turns on and wgpu offers no way to ask for it.
pub const PBR_SAMPLED_TEXTURE_RESERVE: u32 = 48;

/// One class the table lists, with its slot in the material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassSlot {
    /// Index into [`LandTextureTable::arrays`].
    pub array: usize,
    /// Path relative to the converted output root, as the table gives it.
    pub path: String,
    pub format: LandTextureFormat,
    pub size: u32,
    pub layers: u32,
}

impl ClassSlot {
    fn new(array: usize, file: &LandTextureArray) -> Self {
        Self {
            array,
            path: file.path.clone(),
            format: file.format,
            size: file.size,
            layers: file.layers,
        }
    }
}

/// What the material's pipeline specialisation depends on: how many classes are bound, per usage.
/// Every cell of a conversion shares one table, so there is one key per game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TerrainClassKey {
    pub diffuse_classes: u8,
    pub normal_classes: u8,
}

/// The device numbers the refusal check reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterLimits {
    pub max_sampled_textures_per_shader_stage: u32,
    pub max_texture_array_layers: u32,
    pub max_texture_dimension_2d: u32,
}

/// A land texture table, validated and arranged for the material.
#[derive(Debug, Clone)]
pub struct TerrainClasses {
    pub table: LandTextureTable,
    pub diffuse: Vec<ClassSlot>,
    pub normal: Vec<ClassSlot>,
    /// LTEX FormID to table id, built once.
    pub ids_by_form_id: HashMap<u32, u16>,
    /// Per table id: the packed diffuse word `class << 16 | layer` and the packed normal word, or
    /// [`NO_NORMAL_WORD`]. The shader reads these to find an entry's class slot and layer.
    pub entry_words: Vec<[u32; 2]>,
}

impl TerrainClasses {
    /// Validates `table` and assigns its classes to slots.
    pub fn from_table(table: LandTextureTable) -> Result<Self, String> {
        table
            .validate()
            .map_err(|error: TerrainMeshError| format!("land texture table: {error}"))?;
        if table.entries.is_empty() {
            return Err("land texture table lists no textures".to_owned());
        }
        let mut diffuse = Vec::new();
        let mut normal = Vec::new();
        // The table's array index to (usage, slot).
        let mut slot_of_array = Vec::with_capacity(table.arrays.len());
        for (index, file) in table.arrays.iter().enumerate() {
            let slots = match file.usage {
                LandTextureUsage::DiffuseSrgb => &mut diffuse,
                LandTextureUsage::NormalLinear => &mut normal,
            };
            slot_of_array.push((file.usage, slots.len()));
            slots.push(ClassSlot::new(index, file));
        }
        for (usage, slots) in [("diffuse", &diffuse), ("normal", &normal)] {
            if slots.len() > MAX_CLASSES_PER_USAGE {
                return Err(format!(
                    "land texture table lists {} {usage} classes, more than the {MAX_CLASSES_PER_USAGE} the format allows",
                    slots.len()
                ));
            }
        }
        if diffuse.is_empty() {
            return Err("land texture table lists no diffuse class".to_owned());
        }
        let pack = |layer: ArrayLayer| {
            let (_, slot) = slot_of_array[usize::from(layer.array)];
            ((slot as u32) << 16) | u32::from(layer.layer)
        };
        let entry_words = table
            .entries
            .iter()
            .map(|entry| {
                [
                    pack(entry.diffuse),
                    entry.normal.map_or(NO_NORMAL_WORD, pack),
                ]
            })
            .collect();
        let ids_by_form_id = table.ids_by_form_id();
        Ok(Self {
            table,
            diffuse,
            normal,
            ids_by_form_id,
            entry_words,
        })
    }

    pub fn key(&self) -> TerrainClassKey {
        TerrainClassKey {
            diffuse_classes: self.diffuse.len() as u8,
            normal_classes: self.normal.len() as u8,
        }
    }

    /// The sampled textures the material needs on top of Bevy's own: the class slots it always
    /// declares, bound or not.
    pub fn sampled_textures_needed(&self) -> u32 {
        TERRAIN_CLASS_BINDINGS
    }

    /// The lookup table as the bytes of the shader's `array<vec2<u32>>`.
    pub fn entry_bytes(&self) -> Vec<u8> {
        self.entry_words
            .iter()
            .flat_map(|words| words.iter().flat_map(|word| word.to_le_bytes()))
            .collect()
    }

    /// Refuses a table the device cannot bind: too many sampled textures for one shader stage,
    /// an array with more layers than the device allows, or a class larger than the device's
    /// 2D texture size. The message names every number.
    pub fn check_adapter(&self, limits: AdapterLimits) -> Result<(), String> {
        let mut problems = Vec::new();
        let needed = self.sampled_textures_needed() + PBR_SAMPLED_TEXTURE_RESERVE;
        if needed > limits.max_sampled_textures_per_shader_stage {
            problems.push(format!(
                "the terrain material needs {} sampled textures ({} land texture class bindings \
                 plus {} for the standard material and view), the device allows {} per shader stage",
                needed,
                self.sampled_textures_needed(),
                PBR_SAMPLED_TEXTURE_RESERVE,
                limits.max_sampled_textures_per_shader_stage
            ));
        }
        if self.table.max_layers_per_array > limits.max_texture_array_layers {
            problems.push(format!(
                "the table allows {} layers per array, the device only {}",
                self.table.max_layers_per_array, limits.max_texture_array_layers
            ));
        }
        for (usage, slots) in [("diffuse", &self.diffuse), ("normal", &self.normal)] {
            for slot in slots {
                if slot.layers > limits.max_texture_array_layers {
                    problems.push(format!(
                        "{usage} array {} has {} layers, the device allows {}",
                        slot.path, slot.layers, limits.max_texture_array_layers
                    ));
                }
                if slot.size > limits.max_texture_dimension_2d {
                    problems.push(format!(
                        "{usage} array {} is {} texels wide, the device allows {}",
                        slot.path, slot.size, limits.max_texture_dimension_2d
                    ));
                }
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "the land texture arrays ({} diffuse and {} normal classes) cannot be bound: {}",
                self.diffuse.len(),
                self.normal.len(),
                problems.join("; ")
            ))
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use shared::terrain_mesh::{
        CELL_MESH_VERTICES, LandTextureEntry, TERRAIN_MESH_FORMAT_VERSION, TerrainCellMesh,
    };

    pub(crate) fn array(
        usage: LandTextureUsage,
        format: LandTextureFormat,
        size: u32,
        layers: u32,
    ) -> LandTextureArray {
        let usage_name = match usage {
            LandTextureUsage::DiffuseSrgb => "diffuse",
            LandTextureUsage::NormalLinear => "normal",
        };
        LandTextureArray {
            path: format!("terrain/land-{usage_name}-{format:?}-{size}.ktx2").to_lowercase(),
            usage,
            format,
            size,
            layers,
        }
    }

    /// A vanilla-shaped table: diffuse BC1 and BC3 at 1024 and BC1 at 2048, normal BC3 at 1024.
    /// Entry 0 is the default texture; entry `i` is LTEX `0xFF + i`. Even entries have a normal
    /// map, and entry 3 sits in the 2048 class.
    pub(crate) fn vanilla_table(textures: u16) -> LandTextureTable {
        let arrays = vec![
            array(
                LandTextureUsage::DiffuseSrgb,
                LandTextureFormat::Bc1,
                1024,
                64,
            ),
            array(
                LandTextureUsage::DiffuseSrgb,
                LandTextureFormat::Bc3,
                1024,
                64,
            ),
            array(
                LandTextureUsage::DiffuseSrgb,
                LandTextureFormat::Bc1,
                2048,
                2,
            ),
            array(
                LandTextureUsage::NormalLinear,
                LandTextureFormat::Bc3,
                1024,
                64,
            ),
        ];
        LandTextureTable {
            format_version: TERRAIN_MESH_FORMAT_VERSION,
            max_layers_per_array: 2048,
            settings: None,
            arrays,
            entries: (0..textures)
                .map(|id| LandTextureEntry {
                    id,
                    ltex_form_id: if id == 0 { 0 } else { 0xFF + u32::from(id) },
                    diffuse: match id {
                        3 => ArrayLayer { array: 2, layer: 1 },
                        id if id % 3 == 1 => ArrayLayer {
                            array: 1,
                            layer: id,
                        },
                        id => ArrayLayer {
                            array: 0,
                            layer: id,
                        },
                    },
                    normal: (id % 2 == 0).then_some(ArrayLayer {
                        array: 3,
                        layer: id,
                    }),
                })
                .collect(),
        }
    }

    pub(crate) fn mesh_with_slots(stride: u8) -> TerrainCellMesh {
        let stride_len = usize::from(stride);
        TerrainCellMesh {
            cell_id: 1,
            key: [0; 32],
            heights: vec![0.0; CELL_MESH_VERTICES],
            normals: vec![0; 3 * CELL_MESH_VERTICES],
            vertex_colors: Vec::new(),
            slot_stride: stride,
            quadrant_slots: vec![0; 4 * stride_len],
            quadrant_slot_counts: [1; 4],
            weights: vec![0; stride_len * CELL_MESH_VERTICES],
        }
    }

    #[test]
    fn classes_get_dense_slots_per_usage_in_table_order() {
        let classes = TerrainClasses::from_table(vanilla_table(6)).unwrap();
        assert_eq!(classes.diffuse.len(), 3);
        assert_eq!(classes.normal.len(), 1);
        assert_eq!(classes.diffuse[2].size, 2048);
        assert_eq!(
            classes.normal[0].array, 3,
            "slot keeps its table array index"
        );
        assert_eq!(
            classes.key(),
            TerrainClassKey {
                diffuse_classes: 3,
                normal_classes: 1
            }
        );
    }

    #[test]
    fn entry_words_pack_the_class_slot_and_layer() {
        let classes = TerrainClasses::from_table(vanilla_table(6)).unwrap();
        // Entry 3: diffuse in the 2048 class (diffuse slot 2) layer 1, odd id so no normal.
        assert_eq!(classes.entry_words[3], [(2 << 16) | 1, NO_NORMAL_WORD]);
        // Entry 4: id % 3 == 1 puts it in the BC3 class (slot 1) layer 4; even id has a normal.
        assert_eq!(classes.entry_words[4], [(1 << 16) | 4, 4]);
        // The default texture: slot 0 layer 0, with a normal at slot 0 layer 0.
        assert_eq!(classes.entry_words[0], [0, 0]);
        let bytes = classes.entry_bytes();
        assert_eq!(bytes.len(), 6 * 8);
        assert_eq!(
            u32::from_le_bytes(bytes[3 * 8..3 * 8 + 4].try_into().unwrap()),
            (2 << 16) | 1
        );
        assert_eq!(
            u32::from_le_bytes(bytes[3 * 8 + 4..3 * 8 + 8].try_into().unwrap()),
            NO_NORMAL_WORD
        );
    }

    #[test]
    fn form_ids_map_to_table_ids() {
        let classes = TerrainClasses::from_table(vanilla_table(6)).unwrap();
        assert_eq!(classes.ids_by_form_id[&0], 0);
        assert_eq!(classes.ids_by_form_id[&(0xFF + 5)], 5);
        assert!(!classes.ids_by_form_id.contains_key(&0x0BAD));
    }

    #[test]
    fn a_broken_or_empty_table_is_refused() {
        let mut table = vanilla_table(3);
        table.entries[1].id = 2;
        let message = TerrainClasses::from_table(table).unwrap_err();
        assert!(message.contains("land texture table"), "{message}");
        let mut empty = vanilla_table(3);
        empty.entries.clear();
        assert!(
            TerrainClasses::from_table(empty)
                .unwrap_err()
                .contains("no textures")
        );
    }

    #[test]
    fn every_class_the_contract_allows_fits_the_material() {
        let mut arrays = Vec::new();
        for format in DIFFUSE_FORMATS {
            for size in LAND_TEXTURE_SIZE_CLASSES {
                arrays.push(array(LandTextureUsage::DiffuseSrgb, format, size, 2));
            }
        }
        for format in NORMAL_FORMATS {
            for size in LAND_TEXTURE_SIZE_CLASSES {
                arrays.push(array(LandTextureUsage::NormalLinear, format, size, 2));
            }
        }
        let mut table = vanilla_table(2);
        table.arrays = arrays;
        table.entries[0].diffuse = ArrayLayer { array: 0, layer: 0 };
        table.entries[0].normal = None;
        table.entries[1].diffuse = ArrayLayer { array: 8, layer: 1 };
        table.entries[1].normal = Some(ArrayLayer {
            array: 17,
            layer: 1,
        });
        let classes = TerrainClasses::from_table(table).unwrap();
        assert_eq!(classes.diffuse.len(), MAX_CLASSES_PER_USAGE);
        assert_eq!(classes.normal.len(), MAX_CLASSES_PER_USAGE);
        assert_eq!(classes.entry_words[1], [(8 << 16) | 1, (8 << 16) | 1]);
    }

    fn roomy() -> AdapterLimits {
        AdapterLimits {
            max_sampled_textures_per_shader_stage: 1_000_000,
            max_texture_array_layers: 2048,
            max_texture_dimension_2d: 16_384,
        }
    }

    #[test]
    fn the_adapter_check_names_every_limit_that_is_exceeded() {
        let classes = TerrainClasses::from_table(vanilla_table(6)).unwrap();
        classes.check_adapter(roomy()).unwrap();

        let tight = AdapterLimits {
            max_sampled_textures_per_shader_stage: 16,
            max_texture_array_layers: 32,
            max_texture_dimension_2d: 1024,
        };
        let message = classes.check_adapter(tight).unwrap_err();
        assert!(message.contains("sampled textures"), "{message}");
        assert!(
            message.contains("device allows 16 per shader stage"),
            "{message}"
        );
        assert!(
            message.contains("2048 layers per array, the device only 32"),
            "{message}"
        );
        assert!(
            message.contains("has 64 layers, the device allows 32"),
            "{message}"
        );
        assert!(message.contains("2048 texels wide"), "{message}");
        assert!(
            message.contains("3 diffuse and 1 normal classes"),
            "{message}"
        );

        // Exactly enough sampled textures passes, one fewer is refused.
        let needed = TERRAIN_CLASS_BINDINGS + PBR_SAMPLED_TEXTURE_RESERVE;
        let edge = AdapterLimits {
            max_sampled_textures_per_shader_stage: needed,
            ..roomy()
        };
        classes.check_adapter(edge).unwrap();
        let below = AdapterLimits {
            max_sampled_textures_per_shader_stage: needed - 1,
            ..roomy()
        };
        assert!(classes.check_adapter(below).is_err());
    }
}
