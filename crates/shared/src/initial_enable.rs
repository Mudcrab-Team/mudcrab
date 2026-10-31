//! Initial reference enable state shared by runtime spawning and offline baking.

use std::collections::{HashMap, HashSet};

const INITIALLY_DISABLED: u32 = 0x800;
const OPPOSITE_OF_PARENT: u32 = 1;
const PLAYER_REFERENCE: u32 = 0x14;

/// Immutable inputs from a winning reference record, separate from runtime state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitialEnableInputs {
    pub header_flags: u32,
    pub parent_id: Option<u32>,
    pub parent_flags: u32,
}

/// Why a parent chain cannot supply an initial enabled state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EnableStateError {
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

/// A resolved state or an unresolved chain (which the runtime keeps hidden).
pub type EnableState = Result<bool, EnableStateError>;

/// One ordered input to a reference's initial state, suitable for cache identity.
/// Reference entries retain all raw flags, even flags ignored by the resolver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnableDecisionInput {
    /// A winning reference row, starting with the requested reference.
    Reference {
        form_id: u32,
        inputs: InitialEnableInputs,
    },
    /// Built-in enabled PlayerRef (0x14), independent of any database row.
    Player,
    /// The chain requested a parent absent from the database.
    MissingParent(u32),
    /// The chain returned to a reference already present in this input list.
    Cycle(u32),
}

/// Memoizes initial state for the immutable package, including unresolved chains.
pub struct EnableStateResolver {
    resolved: HashMap<u32, EnableState>,
    inputs: HashMap<u32, InitialEnableInputs>,
}

impl Default for EnableStateResolver {
    fn default() -> Self {
        Self {
            // The new-game player exists without a plugin REFR row. This bootstrap snapshot
            // assumes that PlayerRef is enabled; it does not model save/script changes.
            resolved: HashMap::from([(PLAYER_REFERENCE, Ok(true))]),
            inputs: HashMap::new(),
        }
    }
}

impl EnableStateResolver {
    /// Return the complete decision chain without loading or resolving more rows.
    /// Returns `None` if this reference has no memoized state, including after a
    /// loader failure. Missing/cyclic chains do have memoized unresolved states.
    /// Build a fresh resolver when package inputs change.
    pub fn decision_inputs(&self, mut form_id: u32) -> Option<Vec<EnableDecisionInput>> {
        self.resolved.get(&form_id)?;
        let mut chain = Vec::new();
        let mut visited = HashSet::new();
        loop {
            if form_id == PLAYER_REFERENCE {
                chain.push(EnableDecisionInput::Player);
                break;
            }
            if !visited.insert(form_id) {
                chain.push(EnableDecisionInput::Cycle(form_id));
                break;
            }
            let Some(inputs) = self.inputs.get(&form_id).copied() else {
                chain.push(EnableDecisionInput::MissingParent(form_id));
                break;
            };
            chain.push(EnableDecisionInput::Reference { form_id, inputs });
            let Some(parent) = inputs.parent_id.filter(|id| *id != 0) else {
                break;
            };
            form_id = parent;
        }
        Some(chain)
    }

    /// Resolve an immutable reference, loading parents only until a memoized state is reached.
    /// Loader failures propagate without memoizing a state; missing/cyclic chains stay unresolved.
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
            self.inputs.insert(form_id, inputs);
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

    /// Build explicit winning-record inputs without sharing resolver logic.
    fn inputs(flags: u32, parent: Option<(u32, u32)>) -> InitialEnableInputs {
        InitialEnableInputs {
            header_flags: flags,
            parent_id: parent.map(|(id, _)| id),
            parent_flags: parent.map_or(0, |(_, flags)| flags),
        }
    }

    /// Resolve fixture rows in their stated order, including memoized parent lookups.
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
    /// Pin header, parent override, ignored flags, chain inversion and zero-parent rules.
    fn parent_state_overrides_child_flags_and_only_bit_zero_inverts() {
        assert_eq!(
            resolve_all(&[
                (1, inputs(2048, None)),
                (2, inputs(0, Some((1, 0)))),
                (3, inputs(0, Some((1, 1)))),
                (4, inputs(0, None)),
                (5, inputs(0, Some((4, 0x38bf_0d02)))),
                (6, inputs(2048, Some((4, 0)))),
                (7, inputs(2048, Some((3, 0)))),
                (8, inputs(0, Some((3, 1)))),
                (9, inputs(0, Some((0, 1)))),
                (10, inputs(2048, Some((0, 1)))),
                (11, inputs(0x4000, None)),
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
                Ok(true),
                Ok(false),
                Ok(true)
            ]
        );
    }

    #[test]
    /// Unresolved parents stay unresolved even through opposite-state children.
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
        assert_eq!(states[1], Err(EnableStateError::MissingParent(99)));
        assert_eq!(states[2], Err(EnableStateError::Cycle(3)));
        assert_eq!(states[3], Err(EnableStateError::Cycle(3)));
        assert_eq!(states[4], Err(EnableStateError::Cycle(3)));
        assert_eq!(states[5], Err(EnableStateError::Cycle(6)));
    }

    #[test]
    /// Long modded chains must not recurse or repeatedly load memoized parents.
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
        let chain = resolver.decision_inputs(0x0100_0000).unwrap();
        assert_eq!(chain.len(), 100_001);
        assert_eq!(
            chain.last(),
            Some(&EnableDecisionInput::Reference {
                form_id: 0x0101_86a0,
                inputs: InitialEnableInputs {
                    header_flags: 0,
                    parent_id: None,
                    parent_flags: 0
                },
            })
        );
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
    /// PlayerRef is enabled without a plugin row, and child inversion still applies.
    fn new_game_player_parent_is_enabled_without_a_plugin_row() {
        assert_eq!(
            resolve_all(&[
                (1, inputs(2048, Some((0x14, 0)))),
                (2, inputs(0, Some((0x14, 1)))),
            ]),
            [Ok(true), Ok(false)]
        );
    }

    #[test]
    /// A transient parent read error must leave the chain retryable.
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

    #[test]
    /// Memoization must preserve every raw decision input for child cache identities.
    fn decision_inputs_include_memoized_ancestors_and_raw_flags() {
        let mut resolver = EnableStateResolver::default();
        let parent = inputs(2048, None);
        let middle = inputs(0x4000, Some((100, 3)));
        let child = inputs(2048, Some((200, 0x38bf_0d02)));
        assert_eq!(resolver.decision_inputs(100), None);
        assert_eq!(
            resolver.resolve(100, parent, |_| Ok::<_, ()>(None)),
            Ok(Ok(false))
        );
        assert_eq!(
            resolver.resolve(200, middle, |_| -> Result<_, ()> {
                panic!("cached parent must not load")
            }),
            Ok(Ok(true))
        );
        assert_eq!(
            resolver.resolve(300, child, |_| -> Result<_, ()> {
                panic!("cached middle must not load")
            }),
            Ok(Ok(true))
        );
        assert_eq!(
            resolver.decision_inputs(300),
            Some(vec![
                EnableDecisionInput::Reference {
                    form_id: 300,
                    inputs: InitialEnableInputs {
                        header_flags: 2048,
                        parent_id: Some(200),
                        parent_flags: 0x38bf_0d02,
                    }
                },
                EnableDecisionInput::Reference {
                    form_id: 200,
                    inputs: InitialEnableInputs {
                        header_flags: 0x4000,
                        parent_id: Some(100),
                        parent_flags: 3,
                    }
                },
                EnableDecisionInput::Reference {
                    form_id: 100,
                    inputs: InitialEnableInputs {
                        header_flags: 2048,
                        parent_id: None,
                        parent_flags: 0,
                    }
                },
            ])
        );
    }

    #[test]
    /// Missing and cycle terminals are explicit, independent of memoized error origin.
    fn decision_inputs_pin_missing_and_cycle_terminals() {
        let mut resolver = EnableStateResolver::default();
        let missing = inputs(0, Some((99, 1)));
        assert_eq!(
            resolver.resolve(1, missing, |_| Ok::<_, ()>(None)),
            Ok(Err(EnableStateError::MissingParent(99)))
        );
        assert_eq!(
            resolver.decision_inputs(1),
            Some(vec![
                EnableDecisionInput::Reference {
                    form_id: 1,
                    inputs: InitialEnableInputs {
                        header_flags: 0,
                        parent_id: Some(99),
                        parent_flags: 1,
                    }
                },
                EnableDecisionInput::MissingParent(99),
            ])
        );
        assert_eq!(resolver.decision_inputs(99), None);
        let a = inputs(2048, Some((4, 1)));
        let b = inputs(0, Some((3, 0)));
        assert_eq!(
            resolver.resolve(3, a, |id| Ok::<_, ()>(match id {
                3 => Some(a),
                4 => Some(b),
                _ => None,
            })),
            Ok(Err(EnableStateError::Cycle(3)))
        );
        assert_eq!(
            resolver.decision_inputs(4),
            Some(vec![
                EnableDecisionInput::Reference {
                    form_id: 4,
                    inputs: InitialEnableInputs {
                        header_flags: 0,
                        parent_id: Some(3),
                        parent_flags: 0,
                    }
                },
                EnableDecisionInput::Reference {
                    form_id: 3,
                    inputs: InitialEnableInputs {
                        header_flags: 2048,
                        parent_id: Some(4),
                        parent_flags: 1,
                    }
                },
                EnableDecisionInput::Cycle(4),
            ])
        );
    }

    #[test]
    /// Built-in player, zero parent and failed loads retain distinct chain identities.
    fn decision_inputs_distinguish_bootstrap_zero_parent_and_read_failure() {
        let mut resolver = EnableStateResolver::default();
        assert_eq!(
            resolver.resolve(0x14, inputs(2048, Some((99, 1))), |_| -> Result<_, ()> {
                panic!("PlayerRef ignores database inputs")
            }),
            Ok(Ok(true))
        );
        assert_eq!(
            resolver.decision_inputs(0x14),
            Some(vec![EnableDecisionInput::Player])
        );
        assert_eq!(
            resolver.resolve(7, inputs(2048, Some((0x14, 1))), |_| -> Result<_, ()> {
                panic!("built-in parent must not load")
            }),
            Ok(Ok(false))
        );
        assert_eq!(
            resolver.decision_inputs(7),
            Some(vec![
                EnableDecisionInput::Reference {
                    form_id: 7,
                    inputs: InitialEnableInputs {
                        header_flags: 2048,
                        parent_id: Some(0x14),
                        parent_flags: 1,
                    }
                },
                EnableDecisionInput::Player,
            ])
        );
        let zero = InitialEnableInputs {
            header_flags: 2048,
            parent_id: Some(0),
            parent_flags: 9,
        };
        assert_eq!(
            resolver.resolve(5, zero, |_| -> Result<_, ()> {
                panic!("zero parent must not load")
            }),
            Ok(Ok(false))
        );
        assert_eq!(
            resolver.decision_inputs(5),
            Some(vec![EnableDecisionInput::Reference {
                form_id: 5,
                inputs: InitialEnableInputs {
                    header_flags: 2048,
                    parent_id: Some(0),
                    parent_flags: 9,
                },
            }])
        );
        assert_eq!(
            resolver.resolve(6, inputs(0, Some((99, 0))), |_| Err("read failed")),
            Err("read failed")
        );
        assert_eq!(resolver.decision_inputs(6), None);
    }
}
