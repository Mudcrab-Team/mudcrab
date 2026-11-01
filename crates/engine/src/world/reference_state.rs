//! Immutable enable metadata validation used by certified package loading.

use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
};

use bevy::prelude::*;
use color_eyre::{Result, eyre::WrapErr};
use rusqlite::Connection;

use super::enable_state::{
    EnableState, EnableStateError, EnableStateResolver, InitialEnableInputs,
};

/// Immutable enable definitions and initial resolution for one winning-record database.
pub struct EnableCatalog {
    initial: EnableStateResolver,
}

impl EnableCatalog {
    /// Reads the winning reference table once; runtime resolution performs no database IO.
    pub fn read(connection: &Connection) -> Result<Self> {
        let count: usize = connection.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('references') \
             WHERE name IN ('header_flags','enable_parent_id','enable_parent_flags')",
            [],
            |row| row.get(0),
        )?;
        let columns = match count {
            0 => {
                warn!(
                    "world database has no reference enable-state metadata; references default to enabled; reconvert assets to filter initially disabled references"
                );
                "0,NULL,0"
            }
            3 => "header_flags,enable_parent_id,enable_parent_flags",
            _ => color_eyre::eyre::bail!(
                "incomplete reference enable-state columns: expected header_flags, enable_parent_id and enable_parent_flags"
            ),
        };
        let mut statement = connection.prepare(&format!(
            "SELECT id,{columns} FROM \"references\" ORDER BY id"
        ))?;
        let mut definitions = HashMap::new();
        let mut initial = EnableStateResolver::default();
        let mut rows = statement.query([])?;
        let mut order = Vec::new();
        while let Some(row) = rows.next()? {
            let reference = row.get(0)?;
            let inputs = super::database::map_enable_inputs(row, 1)
                .wrap_err_with(|| format!("enable metadata of reference {reference:08X}"))?;
            definitions.insert(reference, inputs);
            order.push(reference);
        }
        let mut diagnosed = HashSet::new();
        for reference in order {
            let state = resolve_definition(&mut initial, &definitions, reference);
            if let Err(error) = state
                && diagnosed.insert(error)
            {
                warn!(reference, %error, "unresolved reference enable state stays hidden");
            }
        }
        Ok(Self { initial })
    }

    /// Initial state, unaffected by session overrides; unresolved or absent IDs return `None`.
    pub fn enabled(&self, reference: u32) -> Option<bool> {
        self.initial.cached(reference).and_then(Result::ok)
    }
}

fn resolve_definition(
    resolver: &mut EnableStateResolver,
    definitions: &HashMap<u32, InitialEnableInputs>,
    reference: u32,
) -> EnableState {
    if let Some(state) = resolver.cached(reference) {
        return state;
    }
    // Only known references are invalidated; PlayerRef always receives an explicit seed.
    let Some(inputs) = definitions.get(&reference).copied() else {
        return Err(EnableStateError::MissingParent(reference));
    };
    resolver
        .resolve(reference, inputs, |parent| {
            Ok::<_, Infallible>(definitions.get(&parent).copied())
        })
        .unwrap_or_else(|never| match never {})
}

/// Effective enable-state change, separate from permanent pose or gameplay revocations.
#[derive(Event, Clone, Copy, Debug)]
pub struct ReferenceEnableChanged {
    pub reference: u32,
    pub enabled: bool,
}
