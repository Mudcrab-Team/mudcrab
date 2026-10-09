//! Perk alternatives selected from their own effect's native discriminator bytes.
use super::{Context, FieldSchema, Selection};

/// A source subrecord in the bounded current PERK effect's physical sequence.
type EffectSubrecord<'a> = ([u8; 4], &'a [u8]);

/// Explicit selector registry used during build validation and runtime decoding.
pub const NAMES: &[&str] = &["perks_effect_data", "perks_function_parameters"];

/// Bound dependencies to the current PRKE/PRKF effect rather than an earlier sibling.
fn current_effect<'a>(context: &Context<'a>) -> Result<&'a [EffectSubrecord<'a>], String> {
    if context.record_type != *b"PERK" {
        return Err("perk selector requires a PERK record".into());
    }
    let start = context
        .preceding_subrecords
        .iter()
        .rposition(|(tag, _)| tag == b"PRKE" || tag == b"PRKF")
        .ok_or("perk effect has no preceding PRKE")?;
    let effect = &context.preceding_subrecords[start..];
    if effect[0].0 != *b"PRKE" || effect[0].1.len() != 3 {
        return Err("perk effect requires its own three-byte PRKE".into());
    }
    Ok(effect)
}

/// Interpret DATA by effect type and EPFD by parameter type plus entry function.
pub fn select(
    name: &str,
    field: &FieldSchema,
    _bytes: &[u8],
    context: &Context<'_>,
) -> Result<Selection, String> {
    let effect = current_effect(context)?;
    let effect_type = effect[0].1[0];
    let selected = match name {
        "perks_effect_data" => match effect_type {
            0 => "quest_stage",
            1 => "ability",
            2 => "entry_point",
            _ => return Err(format!("unknown perk effect type {effect_type}")),
        },
        "perks_function_parameters" => {
            if effect_type != 2 {
                return Err("perk function parameters require an entry-point effect".into());
            }
            let entry = effect
                .iter()
                .find(|(tag, _)| tag == b"DATA")
                .map(|(_, payload)| *payload)
                .filter(|payload| payload.len() == 3)
                .ok_or("perk function parameters require their own three-byte entry DATA")?;
            let parameter_type = effect
                .iter()
                .rfind(|(tag, _)| tag == b"EPFT")
                .map(|(_, payload)| *payload)
                .filter(|payload| payload.len() == 1)
                .ok_or("perk function parameters require their own one-byte EPFT")?[0];
            match parameter_type {
                0 => "unknown_none_data",
                1 => "float",
                2 if matches!(entry[1], 5 | 12 | 13 | 14) => "actor_value_multiplier",
                2 => "float_pair",
                3 => "leveled_item",
                4 => "activation_spell",
                5 => "spell",
                6 => "text",
                7 => "localized_text",
                _ => return Err(format!("unknown perk parameter type {parameter_type}")),
            }
        }
        _ => return Err(format!("unknown perk decider {name}")),
    };
    field
        .fields
        .iter()
        .position(|alternative| alternative.name == selected)
        .map(Selection::Alternative)
        .ok_or_else(|| format!("perk schema lacks {selected} alternative"))
}
