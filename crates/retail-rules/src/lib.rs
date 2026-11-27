//! Pure retail Skyrim rules that Mudcrab must reproduce exactly.
//!
//! Each function here states one rule from the Mudcrab retail-rule registry in our own words and
//! cites the rule's ID (for example `RF-NPC-004`). The engine calls
//! [`scale::placed_reference_scale`] and [`placement::placed_reference_rotation`] for placed actor
//! references (#663), so the code that the proofs cover is the code that runs.
//!
//! The crate has three limits on purpose:
//!
//! - **No dependencies.** The [Kani](https://model-checking.github.io/kani/) model checker compiles
//!   every dependency, so a pure crate keeps each proof small and fast.
//! - **Scalar inputs only.** The functions take plain numbers and flags, not Bevy, glam or database
//!   types. The caller reads game data and passes the values in.
//! - **No engine policy.** A function gives the retail result, even when the engine later guards it
//!   (for example, the engine replaces a zero scale with 1.0). The guard stays in the engine.
//!
//! # Proofs
//!
//! Each module has a `proofs` module that is compiled only by Kani (`#[cfg(kani)]`). A Kani proof
//! checks a property for every input in its stated range, not only for sampled inputs. Run them with
//! `cargo kani -p retail-rules`. The skill in `.agents/skills/verify-retail-rules/SKILL.md` tells
//! how to run them, how to read a failure, and how to keep them current.
//!
//! A proof shows that the code equals the rule as the registry states it. It does not show that
//! the registry equals the retail game. Each module lists the assumptions that only a retail value
//! table can settle.

#![warn(clippy::wildcard_enum_match_arm)]

pub mod placement;
pub mod scale;
