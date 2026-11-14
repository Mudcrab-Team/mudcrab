//! Per-asset-kind reuse decisions for a metadata rebuild (`--reuse-assets`).
//!
//! Each converted asset kind has its own byte contract and its own recorded settings proof in
//! the source manifest. The rebuild decides every kind on its own, so a change that touches one
//! kind no longer rejects the whole package. A kind is reused only when its recorded proof
//! equals the current one. A kind whose bytes the rebuild can regenerate from game data without
//! the normal conversion's extraction, mesh or texture stages is rebuilt. Every other changed
//! kind refuses the rebuild and names the normal conversion as the way to regenerate it.
//!
//! | Kind  | Recorded proof against the current one                        | Decision |
//! |-------|---------------------------------------------------------------|----------|
//! | `nif` | equal                                                         | reuse    |
//! | `nif` | absent or different (mesh contract, producer)                 | refuse   |
//! | `dds` | equal, or absent (the combined settings proof still binds it) | reuse    |
//! | `dds` | different (encoder, quality, levels, producer)                | refuse   |
//! | `pex` | equal, and the source script ABI is the current ABI           | reuse    |
//! | `pex` | anything else (older ABI, different or absent proof)          | rebuild  |
//!
//! A rebuild never trusts the bytes it replaces. It converts the scripts again from the Data
//! scripts and then requires every recorded script source hash to match.

use color_eyre::{Result, eyre::bail};
use std::{collections::BTreeMap, fmt};

/// The asset kinds that carry their own settings proof in the manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Kind {
    /// Meshes (`.nif` sources, `.glb` outputs).
    Nif,
    /// Textures (`.dds` sources, `.ktx2` outputs).
    Dds,
    /// Scripts (`.pex` sources, `.luau` outputs).
    Pex,
}

impl Kind {
    pub(crate) const ALL: [Kind; 3] = [Kind::Nif, Kind::Dds, Kind::Pex];

    /// The key of this kind in the manifest's `asset_configuration_hashes`.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Kind::Nif => "nif",
            Kind::Dds => "dds",
            Kind::Pex => "pex",
        }
    }

    /// Whether the metadata rebuild can regenerate this kind from Data on its own.
    /// Meshes and textures need the normal conversion's extraction and encoder stages.
    pub(crate) fn rebuildable(self) -> bool {
        matches!(self, Kind::Pex)
    }
}

/// What the rebuild does with one kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// Copy the kind's verified bytes from the source pack.
    Reuse,
    /// Convert the kind again from Data and record the new proof.
    Rebuild,
}

impl Action {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Action::Reuse => "reused",
            Action::Rebuild => "rebuilt",
        }
    }
}

/// Why a kind is refused. The text is the error a user reads.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Refusal(pub(crate) String);

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Decide one kind from its recorded proof, the current proof and the source script ABI.
///
/// `source_abi_is_current` only matters for `pex`; it says the combined settings proof of the
/// source records the current script ABI.
pub(crate) fn decide(
    kind: Kind,
    recorded: Option<&str>,
    current: &str,
    source_abi_is_current: bool,
) -> Result<Action, Refusal> {
    match kind {
        Kind::Nif => match recorded {
            Some(recorded) if recorded == current => Ok(Action::Reuse),
            _ => Err(Refusal(
                "retained mesh settings predate the current mesh output contract; \
                 run normal conversion to regenerate models"
                    .into(),
            )),
        },
        Kind::Dds => match recorded {
            Some(recorded) if recorded != current => Err(Refusal(
                "retained texture settings differ from the rebuild settings; \
                 run normal conversion to regenerate textures"
                    .into(),
            )),
            _ => Ok(Action::Reuse),
        },
        Kind::Pex => {
            if source_abi_is_current && recorded.is_none_or(|recorded| recorded == current) {
                Ok(Action::Reuse)
            } else {
                Ok(Action::Rebuild)
            }
        }
    }
}

/// The decisions for every kind of one source pack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Plan {
    actions: BTreeMap<Kind, Action>,
}

impl Plan {
    /// Decide every kind. The first refused kind (mesh, texture, script order) fails the plan.
    pub(crate) fn decide(
        recorded: &BTreeMap<String, String>,
        current: impl Fn(Kind) -> Result<String>,
        source_abi_is_current: bool,
    ) -> Result<Self> {
        let mut actions = BTreeMap::new();
        for kind in Kind::ALL {
            let current = current(kind)?;
            match decide(
                kind,
                recorded.get(kind.key()).map(String::as_str),
                &current,
                source_abi_is_current,
            ) {
                Ok(action) => {
                    debug_assert!(action == Action::Reuse || kind.rebuildable());
                    actions.insert(kind, action);
                }
                Err(refusal) => bail!("{refusal}"),
            }
        }
        Ok(Self { actions })
    }

    pub(crate) fn action(&self, kind: Kind) -> Action {
        self.actions[&kind]
    }

    pub(crate) fn rebuilds(&self, kind: Kind) -> bool {
        self.action(kind) == Action::Rebuild
    }

    /// Per-kind outcomes, for the rebuild provenance.
    pub(crate) fn outcomes(&self) -> impl Iterator<Item = (Kind, Action)> + '_ {
        self.actions.iter().map(|(kind, action)| (*kind, *action))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recorded(nif: &str, dds: &str, pex: &str) -> BTreeMap<String, String> {
        [("nif", nif), ("dds", dds), ("pex", pex)]
            .into_iter()
            .map(|(kind, hash)| (kind.to_owned(), hash.to_owned()))
            .collect()
    }

    fn current(kind: Kind) -> Result<String> {
        Ok(format!("current-{}", kind.key()))
    }

    /// Every row of the decision table in the module documentation.
    #[test]
    fn decision_table() {
        use Action::{Rebuild, Reuse};
        // (kind, recorded, source ABI is current, expected)
        type Row<'a> = (Kind, Option<&'a str>, bool, Result<Action, ()>);
        let rows: [Row; 12] = [
            (Kind::Nif, Some("current-nif"), true, Ok(Reuse)),
            (Kind::Nif, Some("other"), true, Err(())),
            (Kind::Nif, None, true, Err(())),
            (Kind::Dds, Some("current-dds"), true, Ok(Reuse)),
            (Kind::Dds, None, true, Ok(Reuse)),
            (Kind::Dds, Some("other"), true, Err(())),
            (Kind::Pex, Some("current-pex"), true, Ok(Reuse)),
            (Kind::Pex, None, true, Ok(Reuse)),
            (Kind::Pex, Some("older-abi"), false, Ok(Rebuild)),
            (Kind::Pex, None, false, Ok(Rebuild)),
            // A script proof that disagrees with its own combined proof is never trusted.
            (Kind::Pex, Some("other"), true, Ok(Rebuild)),
            (Kind::Pex, Some("current-pex"), false, Ok(Rebuild)),
        ];
        for (kind, recorded, abi_current, expected) in rows {
            let actual = decide(
                kind,
                recorded,
                &format!("current-{}", kind.key()),
                abi_current,
            );
            assert_eq!(
                actual.map_err(|_| ()),
                expected,
                "{kind:?} recorded={recorded:?} abi_current={abi_current}"
            );
        }
    }

    /// Only scripts can be regenerated by the metadata rebuild today.
    #[test]
    fn only_scripts_are_rebuildable() {
        assert_eq!(
            Kind::ALL
                .into_iter()
                .filter(|kind| kind.rebuildable())
                .collect::<Vec<_>>(),
            [Kind::Pex]
        );
    }

    #[test]
    fn a_script_abi_bump_rebuilds_scripts_and_reuses_the_rest() {
        let plan = Plan::decide(
            &recorded("current-nif", "current-dds", "old-abi-pex"),
            current,
            false,
        )
        .unwrap();
        assert_eq!(plan.action(Kind::Nif), Action::Reuse);
        assert_eq!(plan.action(Kind::Dds), Action::Reuse);
        assert_eq!(plan.action(Kind::Pex), Action::Rebuild);
        assert!(plan.rebuilds(Kind::Pex) && !plan.rebuilds(Kind::Nif));
    }

    #[test]
    fn a_current_pack_reuses_every_kind() {
        let plan = Plan::decide(
            &recorded("current-nif", "current-dds", "current-pex"),
            current,
            true,
        )
        .unwrap();
        assert!(plan.outcomes().all(|(_, action)| action == Action::Reuse));
    }

    /// A changed mesh contract or texture setting is never reused or relabeled, even when the
    /// scripts would also rebuild. The refusal names the kind to regenerate.
    #[test]
    fn a_changed_mesh_or_texture_kind_fails_closed() {
        let mesh = Plan::decide(
            &recorded("old", "current-dds", "current-pex"),
            current,
            true,
        )
        .unwrap_err();
        assert!(format!("{mesh}").contains("mesh output contract"), "{mesh}");
        let texture =
            Plan::decide(&recorded("current-nif", "old", "old-abi"), current, false).unwrap_err();
        assert!(
            format!("{texture}").contains("texture settings"),
            "{texture}"
        );
    }
}
