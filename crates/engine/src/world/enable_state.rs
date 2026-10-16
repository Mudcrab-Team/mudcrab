use std::collections::{HashMap, HashSet};

const INITIALLY_DISABLED: u32 = 0x800;
const OPPOSITE_OF_PARENT: u32 = 1;
const PLAYER_REFERENCE: u32 = 0x14;

/// Immutable inputs from a winning reference record, separate from runtime state.
#[derive(Debug, Clone, Copy)]
pub(super) struct InitialEnableInputs {
    pub header_flags: u32,
    pub parent_id: Option<u32>,
    pub parent_flags: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum EnableStateError {
    MissingParent(u32),
    Cycle(u32),
}

impl std::fmt::Display for EnableStateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingParent(id) => write!(f, "missing enable parent {id:08X}"),
            Self::Cycle(id) => write!(f, "enable-parent cycle at {id:08X}"),
        }
    }
}

pub(super) type EnableState = Result<bool, EnableStateError>;

/// Memoizes initial state for the immutable package, including unresolved chains.
pub(super) struct EnableStateResolver {
    resolved: HashMap<u32, EnableState>,
}

impl Default for EnableStateResolver {
    fn default() -> Self {
        Self {
            // The new-game player exists without a plugin REFR row. This bootstrap snapshot
            // assumes that PlayerRef is enabled; it does not model save/script changes.
            resolved: HashMap::from([(PLAYER_REFERENCE, Ok(true))]),
        }
    }
}

impl EnableStateResolver {
    pub fn resolve<E>(
        &mut self,
        mut form_id: u32,
        mut inputs: InitialEnableInputs,
        mut load_parent: impl FnMut(u32) -> Result<Option<InitialEnableInputs>, E>,
    ) -> Result<EnableState, E> {
        let mut path = Vec::new();
        let mut visited = HashSet::new();
        let mut state = loop {
            if let Some(state) = self.resolved.get(&form_id) {
                break *state;
            }
            if !visited.insert(form_id) {
                break Err(EnableStateError::Cycle(form_id));
            }
            let Some(parent) = inputs.parent_id.filter(|id| *id != 0) else {
                path.push((form_id, false));
                break Ok(inputs.header_flags & INITIALLY_DISABLED == 0);
            };
            path.push((form_id, inputs.parent_flags & OPPOSITE_OF_PARENT != 0));
            if let Some(state) = self.resolved.get(&parent) {
                break *state;
            }
            let Some(parent_inputs) = load_parent(parent)? else {
                break Err(EnableStateError::MissingParent(parent));
            };
            form_id = parent;
            inputs = parent_inputs;
        };
        for (id, opposite) in path.into_iter().rev() {
            // Inversion applies to a resolved state only. An unresolved chain stays unresolved.
            state = state.map(|enabled| enabled ^ opposite);
            self.resolved.insert(id, state);
        }
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(flags: u32, parent: Option<(u32, u32)>) -> InitialEnableInputs {
        InitialEnableInputs {
            header_flags: flags,
            parent_id: parent.map(|(id, _)| id),
            parent_flags: parent.map_or(0, |(_, flags)| flags),
        }
    }

    fn resolve_all(rows: &[(u32, InitialEnableInputs)]) -> Vec<EnableState> {
        let definitions: HashMap<_, _> = rows.iter().copied().collect();
        let mut resolver = EnableStateResolver::default();
        rows.iter()
            .map(|(id, inputs)| {
                resolver
                    .resolve(*id, *inputs, |parent| {
                        Ok::<_, ()>(definitions.get(&parent).copied())
                    })
                    .unwrap()
            })
            .collect()
    }

    #[test]
    fn parent_state_overrides_child_flags_and_only_bit_zero_inverts() {
        assert_eq!(
            resolve_all(&[
                (1, inputs(INITIALLY_DISABLED, None)),
                (2, inputs(0, Some((1, 0)))),
                (3, inputs(0, Some((1, 1)))),
                (4, inputs(0, None)),
                (5, inputs(0, Some((4, 0x38bf_0d02)))),
                (6, inputs(INITIALLY_DISABLED, Some((4, 0)))),
                (7, inputs(INITIALLY_DISABLED, Some((3, 0)))),
                (8, inputs(0, Some((3, 1)))),
                (9, inputs(0, Some((0, 1)))),
            ]),
            [
                Ok(false),
                Ok(false),
                Ok(true),
                Ok(true),
                Ok(true),
                Ok(true),
                Ok(true),
                Ok(false),
                Ok(true)
            ]
        );
    }

    #[test]
    fn missing_parent_and_cycles_stay_unresolved_through_inversion() {
        let states = resolve_all(&[
            (1, inputs(0, Some((99, 1)))),
            (2, inputs(0, Some((1, 1)))),
            (3, inputs(0, Some((4, 0)))),
            (4, inputs(0, Some((3, 1)))),
            (5, inputs(0, Some((3, 1)))),
            (6, inputs(0, Some((6, 1)))),
        ]);
        assert_eq!(states[0], Err(EnableStateError::MissingParent(99)));
        assert_eq!(states[1], states[0]);
        assert_eq!(states[2], Err(EnableStateError::Cycle(3)));
        assert_eq!(states[3], states[2]);
        assert_eq!(states[4], states[2]);
        assert_eq!(states[5], Err(EnableStateError::Cycle(6)));
    }

    #[test]
    fn long_chains_are_iterative_and_memoized() {
        let mut resolver = EnableStateResolver::default();
        let mut reads = 0;
        let state = resolver
            .resolve(0x0100_0000, inputs(0, Some((0x0100_0001, 0))), |id| {
                reads += 1;
                Ok::<_, ()>(Some(inputs(
                    0,
                    (id < 0x0100_0000 + 100_000).then_some((id + 1, 0)),
                )))
            })
            .unwrap();
        assert_eq!(state, Ok(true));
        assert_eq!(reads, 100_000);
        assert_eq!(
            resolver
                .resolve(
                    0x0100_0001,
                    inputs(0, Some((0x0100_0002, 0))),
                    |_| -> Result<_, ()> {
                        panic!("already resolved parents must not be read again")
                    }
                )
                .unwrap(),
            Ok(true)
        );
    }

    #[test]
    fn new_game_player_parent_is_enabled_without_a_plugin_row() {
        assert_eq!(
            resolve_all(&[
                (1, inputs(INITIALLY_DISABLED, Some((PLAYER_REFERENCE, 0)))),
                (2, inputs(0, Some((PLAYER_REFERENCE, 1)))),
            ]),
            [Ok(true), Ok(false)]
        );
    }

    #[test]
    fn parent_read_errors_propagate_and_are_not_cached_as_disabled() {
        let mut resolver = EnableStateResolver::default();
        let child = inputs(0, Some((2, 0)));
        assert_eq!(
            resolver.resolve(1, child, |_| Err("read failed")),
            Err("read failed")
        );
        assert_eq!(
            resolver.resolve(1, child, |_| Ok::<_, &str>(Some(inputs(0, None)))),
            Ok(Ok(true))
        );
    }
}
