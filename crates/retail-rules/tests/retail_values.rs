//! Retail value tables: outputs that the retail game gives for chosen synthetic inputs.
//!
//! A Kani proof shows that the code equals the registry rule. A row here shows that the code
//! equals the retail game for one input, bit for bit.
//!
//! Rules for rows:
//!
//! - Only synthetic inputs, at most [`MAX_ROWS_PER_RULE`] rows per registry ID.
//! - Each row names its registry ID. It names no address and no function from the retail game.
//! - Each table is a tab-separated file in `tests/data/`, named after its registry ID, copied
//!   unchanged from an approved value table. Outputs are hex bits, so a value cannot round on its
//!   way in.
//! - A row that fails is a finding. Do not delete or edit a row to make the test pass.

use retail_rules::scale::sex_index_of_form_type;

/// The most rows that one registry ID may have in this public table.
const MAX_ROWS_PER_RULE: usize = 50;

/// RF-NPC-005: the sex index of a form. Columns: rule_id, form_type_hex, acbs_flags_hex,
/// output_hex, output_dec.
const RF_NPC_005: &str = include_str!("data/RF-NPC-005.tsv");

fn hex(field: &str) -> u32 {
    let digits = field.strip_prefix("0x").unwrap_or(field);
    u32::from_str_radix(digits, 16).unwrap_or_else(|_| panic!("not hex: {field}"))
}

/// The data rows of one table, after the header, split into fields. Each row must name `rule`.
fn rows<'a>(table: &'a str, rule: &str, header: &str) -> Vec<Vec<&'a str>> {
    let mut lines = table.lines();
    assert_eq!(lines.next(), Some(header), "{rule}: unexpected header");
    let rows: Vec<Vec<&str>> = lines
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.split('\t').collect())
        .collect();
    for row in &rows {
        assert_eq!(row[0], rule, "a row in the {rule} table names another rule");
    }
    assert!(
        rows.len() <= MAX_ROWS_PER_RULE,
        "{rule} has {} public rows",
        rows.len()
    );
    rows
}

#[test]
fn rf_npc_005_sex_index_matches_every_retail_row() {
    let rows = rows(
        RF_NPC_005,
        "RF-NPC-005",
        "rule_id\tform_type_hex\tacbs_flags_hex\toutput_hex\toutput_dec",
    );
    assert_eq!(rows.len(), 24, "the approved table has 24 rows");
    let mut failures = Vec::new();
    for row in &rows {
        let [_, form_type, flags, output_hex, output_dec] = row[..] else {
            panic!("RF-NPC-005: a row has {} fields: {row:?}", row.len());
        };
        let form_type = u8::try_from(hex(form_type)).expect("a form type is one byte");
        let expected = hex(output_hex) as i32;
        assert_eq!(output_dec.parse::<i32>().ok(), Some(expected), "{row:?}");
        let actual = sex_index_of_form_type(form_type, hex(flags));
        if actual != expected {
            failures.push(format!("{row:?}: the code gives {actual}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
