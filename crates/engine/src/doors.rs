//! Load doors: the data half of doors on main. The world database's optional `door_links` table
//! says where a door reference leads, and streaming attaches [`LoadDoor`] to every spawned
//! reference that has a usable link. `crate::door_crossing` activates them.

use crate::world::database::DoorLinkRow;
use bevy::prelude::*;
use std::sync::Once;

/// `REFR` record header flag bit 8 (`0x100`), "Inaccessible", on a reference to a `DOOR` (xEdit
/// `wbDefinitionsTES5.pas`, CommonLibSSE-NG `TESObjectREFR::RecordFlag::kInaccessible`; the same
/// bit is "Doesn't Light Water" on a `LIGH` reference). An inaccessible load door cannot be
/// activated or crossed (issue #1027).
pub const REFERENCE_INACCESSIBLE: u32 = 0x100;
/// `DOOR.FNAM` bit 1 (`0x02`), "Automatic Door" (CommonLibSSE-NG `TESObjectDOOR::Flag::kAutomatic`,
/// xEdit `wbDefinitionsTES5.pas`, <https://ck.uesp.net/wiki/Door>): the door transitions when the
/// player approaches/touches it, with no activation key (issue #1039).
pub const DOOR_FLAG_AUTOMATIC: u8 = 0x02;
/// `DOOR.FNAM` bit 2 (`0x04`), "Hidden" (CommonLibSSE-NG `TESObjectDOOR::Flag::kHidden`, xEdit
/// `wbDefinitionsTES5.pas`, <https://ck.uesp.net/wiki/Door>): omits the door from the local map
/// (issue #1039). Secret doors carrying this bit (such as Ustengrav Depths and the Ragged Flagon
/// Cistern entrance) stay activatable with E. Neither a local map nor a door activation prompt
/// exists in this engine yet, so [`LoadDoor::is_hidden`] exposes the flag for them.
pub const DOOR_FLAG_HIDDEN: u8 = 0x04;

/// The `DOOR.FNAM` flags byte of a load door's base record, uninterpreted. It is read from the base
/// `DOOR` record in the world database's `records` table, so no metadata rebuild is needed.
#[derive(Reflect, Debug, Clone, Copy, PartialEq, Eq)]
pub struct DoorFlags(pub u8);

impl DoorFlags {
    pub fn automatic(self) -> bool {
        self.0 & DOOR_FLAG_AUTOMATIC != 0
    }

    pub fn hidden(self) -> bool {
        self.0 & DOOR_FLAG_HIDDEN != 0
    }
}

/// Where a load door leads, as converted from the source door's `XTEL` subrecord.
#[derive(Reflect, Debug, Clone, PartialEq)]
pub struct DoorDestination {
    /// The destination door reference (`XTEL` bytes 0..4, after plugin FormID remapping).
    pub destination_ref_id: u32,
    /// `Some(cell_id)` when the destination is an interior cell.
    pub interior_cell_id: Option<u32>,
    /// `Some(worldspace_id)` when the destination is in a worldspace. Exactly one of
    /// `interior_cell_id` and `worldspace_id` is `Some` for a usable door.
    pub worldspace_id: Option<u32>,
    /// Arrival position in Creation-engine units (`XTEL` bytes 4..16). This is not the destination
    /// door's own position.
    pub arrival_position: [f32; 3],
    /// Arrival rotation in Creation-engine radians (`XTEL` bytes 16..28).
    pub arrival_rotation: [f32; 3],
}

/// Marks a spawned reference as a load door.
#[derive(Component, Reflect, Debug, Clone, PartialEq)]
#[reflect(Component)]
pub struct LoadDoor {
    /// The door reference's own FormID.
    pub ref_id: u32,
    pub destination: DoorDestination,
    /// The base `DOOR` record's `FNAM` flags. `None` when the world database does not carry them
    /// (an old database, or an unreadable record): the door then behaves as a plain prompt door and
    /// nothing is assumed about its flags.
    pub flags: Option<DoorFlags>,
}

impl LoadDoor {
    /// Whether the door transitions when the player touches it, with no activation key.
    pub fn is_automatic(&self) -> bool {
        self.flags.is_some_and(DoorFlags::automatic)
    }

    /// Whether the base `DOOR` carries `DOOR.FNAM` `Hidden` (`0x04`,
    /// <https://ck.uesp.net/wiki/Door>: omitted from the local map). `false` when the world
    /// database did not supply base door flags.
    pub fn is_hidden(&self) -> bool {
        self.flags.is_some_and(DoorFlags::hidden)
    }
}

static REFERENCE_FLAGS_MISSING: Once = Once::new();
static DOOR_FLAGS_MISSING: Once = Once::new();

/// The [`LoadDoor`] of a reference with this `door_links` row, or `None` when it has no row, the
/// reference is flagged [`REFERENCE_INACCESSIBLE`], or the link's destination cannot be resolved to
/// an interior cell or a worldspace.
///
/// A database that lacks the reference header flags or the base `DOOR` record leaves the matching
/// field `None` on the row. Nothing is invented for it: the door is treated as accessible and
/// non-automatic, and the gap is logged once.
///
/// Both halves of the destination are form ids, and 0 is no form: a `Some(0)` worldspace is as
/// unresolved as a `Some(0)` cell and reads as `None` here (the converter copies whatever
/// `worldspace_id` the destination cell has). The rules below then apply unchanged, so
/// `(0, cell != 0)` is an interior door to that cell, `(0, 0)` and `(0, None)` are no door, and any
/// real worldspace id still leads outside.
pub(crate) fn load_door(ref_id: u32, link: Option<&DoorLinkRow>) -> Option<LoadDoor> {
    let link = link?;
    match link.reference_flags {
        Some(flags) if flags & REFERENCE_INACCESSIBLE != 0 => {
            debug!(
                reference = format_args!("{ref_id:08X}"),
                "inaccessible load door: the reference is not a usable door"
            );
            return None;
        }
        Some(_) => {}
        None => REFERENCE_FLAGS_MISSING.call_once(|| {
            warn!(
                "world database has no reference header flags for door links; inaccessible load \
                 doors cannot be told apart and stay usable (metadata rebuild adds them)"
            );
        }),
    }
    if link.base_flags.is_none() {
        DOOR_FLAGS_MISSING.call_once(|| {
            warn!(
                "world database has no readable DOOR.FNAM for door links; automatic and hidden \
                 load doors behave as plain prompt doors"
            );
        });
    }
    let worldspace_id = link.destination_worldspace_id.filter(|id| *id != 0);
    let interior_cell_id = match (worldspace_id, link.destination_cell_id) {
        (None, Some(cell_id)) if cell_id != 0 => Some(cell_id),
        _ => None,
    };
    if interior_cell_id.is_none() && worldspace_id.is_none() {
        return None;
    }
    Some(LoadDoor {
        ref_id,
        destination: DoorDestination {
            destination_ref_id: link.destination_ref_id,
            interior_cell_id,
            worldspace_id,
            arrival_position: link.arrival_position,
            arrival_rotation: link.arrival_rotation,
        },
        flags: link.base_flags.map(DoorFlags),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(cell: Option<u32>, worldspace: Option<u32>) -> DoorLinkRow {
        DoorLinkRow {
            destination_ref_id: 7,
            destination_cell_id: cell,
            destination_worldspace_id: worldspace,
            arrival_position: [1.0, 2.0, 3.0],
            arrival_rotation: [0.0, 0.0, 0.5],
            reference_flags: Some(0),
            base_flags: Some(0),
        }
    }

    #[test]
    fn an_interior_link_carries_its_cell_and_arrival() {
        let door = load_door(5, Some(&link(Some(99), None))).unwrap();
        assert_eq!(door.ref_id, 5);
        assert_eq!(door.destination.interior_cell_id, Some(99));
        assert_eq!(door.destination.worldspace_id, None);
        assert_eq!(door.destination.arrival_position, [1.0, 2.0, 3.0]);
        assert_eq!(door.destination.arrival_rotation, [0.0, 0.0, 0.5]);
    }

    #[test]
    fn a_worldspace_link_leads_outside() {
        let door = load_door(5, Some(&link(Some(12), Some(60)))).unwrap();
        assert_eq!(door.destination.interior_cell_id, None);
        assert_eq!(door.destination.worldspace_id, Some(60));
        let exterior_only = load_door(5, Some(&link(None, Some(60)))).unwrap();
        assert_eq!(exterior_only.destination.worldspace_id, Some(60));
    }

    #[test]
    fn an_unresolved_or_absent_link_is_no_load_door() {
        assert!(load_door(5, None).is_none());
        assert!(load_door(5, Some(&link(None, None))).is_none());
        assert!(load_door(5, Some(&link(Some(0), None))).is_none());
    }

    #[test]
    fn a_zero_worldspace_leaves_a_real_cell_link_an_interior_door() {
        // Worldspace 0 is never a worldspace, so it reads as unresolved and the cell carries the
        // door, exactly as for a link with a NULL worldspace.
        let door = load_door(5, Some(&link(Some(12), Some(0)))).unwrap();
        assert_eq!(door.destination.interior_cell_id, Some(12));
        assert_eq!(door.destination.worldspace_id, None);
    }

    #[test]
    fn a_zero_worldspace_without_a_real_cell_is_no_load_door() {
        assert!(load_door(5, Some(&link(None, Some(0)))).is_none());
        assert!(load_door(5, Some(&link(Some(0), Some(0)))).is_none());
    }

    #[test]
    fn an_inaccessible_reference_is_no_load_door() {
        // REFR 00027ED0 in the retail pack carries 0x500: Inaccessible with Persistent.
        for flags in [REFERENCE_INACCESSIBLE, 0x500, 0x4000_0100] {
            let mut row = link(Some(99), None);
            row.reference_flags = Some(flags);
            assert!(load_door(5, Some(&row)).is_none(), "flags {flags:#x}");
        }
        for flags in [0, 0x400, 0x200, 0x8000_0000, 0x0000_0800] {
            let mut row = link(Some(99), None);
            row.reference_flags = Some(flags);
            assert!(load_door(5, Some(&row)).is_some(), "flags {flags:#x}");
        }
    }

    #[test]
    fn unknown_reference_flags_leave_the_door_usable() {
        let mut row = link(Some(99), None);
        row.reference_flags = None;
        assert!(load_door(5, Some(&row)).is_some());
    }

    #[test]
    fn door_flags_decode_automatic_and_hidden() {
        let door = |base_flags| {
            let mut row = link(Some(99), None);
            row.base_flags = base_flags;
            load_door(5, Some(&row)).unwrap()
        };
        let plain = door(Some(0x00));
        assert!(!plain.is_automatic() && !plain.is_hidden());
        let automatic = door(Some(DOOR_FLAG_AUTOMATIC));
        assert!(automatic.is_automatic() && !automatic.is_hidden());
        let hidden = door(Some(DOOR_FLAG_HIDDEN));
        assert!(!hidden.is_automatic() && hidden.is_hidden());
        // Retail FNAM 14 (Tolvald's Cave): automatic, hidden and minimal use.
        let both = door(Some(0x0E));
        assert!(both.is_automatic() && both.is_hidden());
        // Flags that need systems this engine lacks change nothing here.
        let other = door(Some(0x08 | 0x10 | 0x20));
        assert!(!other.is_automatic() && !other.is_hidden());
    }

    #[test]
    fn a_door_without_flags_is_a_plain_prompt_door() {
        let mut row = link(Some(99), None);
        row.base_flags = None;
        let door = load_door(5, Some(&row)).unwrap();
        assert_eq!(door.flags, None);
        assert!(!door.is_automatic() && !door.is_hidden());
    }

    /// Guard: the flag bits this module reads must match the converter's decoded `DOOR.FNAM` flags.
    /// Every flag the schema decodes is either handled here or listed as not yet implemented, so a
    /// new schema flag fails this test until someone decides what the engine does with it. The
    /// not-implemented list mirrors `tools/flag_inventory/dispositions.json`.
    #[test]
    fn door_fnam_bits_match_the_converter_schema() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../converter/src/records/schema.json")).unwrap();
        let door = schema["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["signature"] == "DOOR")
            .expect("schema has a DOOR record");
        let fnam = door["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|field| field["signature"] == "FNAM")
            .expect("DOOR has an FNAM");
        let flags = fnam["flags"].as_object().unwrap();
        let name_of = |bit: u8| flags[&bit.to_string()].as_str().unwrap().to_owned();
        assert_eq!(name_of(DOOR_FLAG_AUTOMATIC), "automatic");
        assert_eq!(name_of(DOOR_FLAG_HIDDEN), "hidden");
        let mut unhandled: Vec<(u8, &str)> = flags
            .iter()
            .map(|(bit, name)| (bit.parse().unwrap(), name.as_str().unwrap()))
            .filter(|(bit, _)| *bit != DOOR_FLAG_AUTOMATIC && *bit != DOOR_FLAG_HIDDEN)
            .collect();
        unhandled.sort();
        assert_eq!(
            unhandled,
            [
                (0x08, "minimal_use"),
                (0x10, "sliding"),
                (0x20, "no_combat_search_open"),
            ],
            "a new DOOR.FNAM flag needs an engine decision and a flag_inventory disposition"
        );
    }

    /// Guard: the reference flag this module reads is the catalogued Inaccessible bit.
    #[test]
    fn the_inaccessible_bit_matches_the_flag_catalog() {
        let catalog: serde_json::Value =
            serde_json::from_str(include_str!("../../../tools/flag_inventory/catalog.json"))
                .unwrap();
        let entry = catalog
            .as_array()
            .unwrap()
            .iter()
            .find(|flag| flag["id"] == "REFR.DOOR.RecordHeader.bit8")
            .expect("catalog has the REFR[DOOR] Inaccessible flag");
        assert_eq!(entry["name"], "Inaccessible");
        assert_eq!(
            1u32 << entry["bit"].as_u64().unwrap(),
            REFERENCE_INACCESSIBLE
        );
    }
}
