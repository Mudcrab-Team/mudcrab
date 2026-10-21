//! Actor components for referenced `NPC_` bases.
//!
//! The converter publishes one `actor_models` row per referenced `NPC_` base and one
//! `actor_model_parts` row per biped slot (db-schema.md §18). The engine turns a placed reference
//! to such a base into one actor entity: the reference keeps its authored transform, carries these
//! components, and draws the composed body GLB as its visual root.
//!
//! Head attachment and animation are later slices; this module only describes the actor and its
//! authored parts.

use bevy::prelude::*;
use std::fmt;

/// The composed body the converter publishes for the shared humanoid skeleton, by sex.
pub fn composed_body_scene(sex: ActorSex) -> &'static str {
    match sex {
        ActorSex::Male => "meshes/actors/character/composed/default/male/body.glb",
        ActorSex::Female => "meshes/actors/character/composed/default/female/body.glb",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActorSex {
    Male,
    Female,
}

impl ActorSex {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "male" => Some(Self::Male),
            "female" => Some(Self::Female),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Male => "male",
            Self::Female => "female",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActorResolution {
    Body,
    BodyHead,
    Unresolved,
}

impl ActorResolution {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "body" => Some(Self::Body),
            "body_head" => Some(Self::BodyHead),
            "unresolved" => Some(Self::Unresolved),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Body => "body",
            Self::BodyHead => "body_head",
            Self::Unresolved => "unresolved",
        }
    }

    pub fn is_resolved(self) -> bool {
        !matches!(self, Self::Unresolved)
    }
}

/// A biped slot the race skin resolves (`actor_model_parts.slot`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActorSlot {
    Body,
    Hands,
    Feet,
}

impl ActorSlot {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "body" => Some(Self::Body),
            "hands" => Some(Self::Hands),
            "feet" => Some(Self::Feet),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Body => "body",
            Self::Hands => "hands",
            Self::Feet => "feet",
        }
    }

    /// The slot's position in the authored order, so a parts list is deterministic.
    pub fn index(self) -> usize {
        match self {
            Self::Body => 0,
            Self::Hands => 1,
            Self::Feet => 2,
        }
    }
}

/// Where the catalog's head path came from (`actor_models.head_source`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActorHeadSource {
    Facegen,
    Default,
}

impl ActorHeadSource {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "facegen" => Some(Self::Facegen),
            "default" => Some(Self::Default),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Facegen => "facegen",
            Self::Default => "default",
        }
    }
}

/// One biped part as the catalog authored it. The path is the source NIF path; the composed body
/// scene is what the actor actually draws.
#[derive(Debug, Clone, PartialEq, Eq, Component)]
pub struct ActorPart {
    pub slot: ActorSlot,
    pub model_path: String,
}

impl ActorPart {
    pub fn new(slot: ActorSlot, model_path: impl Into<String>) -> Option<Self> {
        let model_path = model_path.into();
        (!model_path.trim().is_empty()).then_some(Self { slot, model_path })
    }
}

/// Every catalog part of one actor, in slot order.
///
/// Bevy holds at most one component of a given type per entity, so the parts travel together in
/// one component rather than as three `ActorPart` components. No part is spawned as its own scene.
#[derive(Debug, Clone, Default, PartialEq, Eq, Component)]
pub struct ActorParts(Vec<ActorPart>);

impl ActorParts {
    pub fn new(parts: impl IntoIterator<Item = ActorPart>) -> Self {
        let mut parts: Vec<_> = parts.into_iter().collect();
        parts.sort_by_key(|part| part.slot.index());
        Self(parts)
    }

    pub fn iter(&self) -> impl Iterator<Item = &ActorPart> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, slot: ActorSlot) -> Option<&ActorPart> {
        self.0.iter().find(|part| part.slot == slot)
    }
}

/// The head model path the catalog resolved for this actor.
#[derive(Debug, Clone, PartialEq, Eq, Component)]
pub struct ActorHead {
    pub model_path: String,
    pub source: ActorHeadSource,
}

impl ActorHead {
    pub fn new(model_path: impl Into<String>, source: ActorHeadSource) -> Option<Self> {
        let model_path = model_path.into();
        (!model_path.trim().is_empty()).then_some(Self { model_path, source })
    }
}

/// A referenced `NPC_` placed in the world.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Component)]
pub struct Actor {
    /// The base `NPC_` FormID, not the placed reference's.
    pub base_form_id: u32,
    pub race_id: u32,
    pub sex: ActorSex,
    pub resolution: ActorResolution,
}

impl Actor {
    pub fn new(
        base_form_id: u32,
        race_id: u32,
        sex: ActorSex,
        resolution: ActorResolution,
    ) -> Self {
        Self {
            base_form_id,
            race_id,
            sex,
            resolution,
        }
    }
}

/// Why an actor reference was not spawned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorSpawnSkip {
    /// The catalog recorded `resolution = 'unresolved'`.
    Unresolved,
    /// Neither the composed body asset nor a usable `body` part path resolves.
    NoBody,
}

impl ActorSpawnSkip {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unresolved => "resolution is unresolved",
            Self::NoBody => "no composed body asset or body part model",
        }
    }
}

/// The one diagnostic emitted for a skipped actor, naming its base FormID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActorSpawnDiagnostic {
    pub base_form_id: u32,
    pub reason: ActorSpawnSkip,
}

impl fmt::Display for ActorSpawnDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "NPC_ {:08X}: {}",
            self.base_form_id,
            self.reason.as_str()
        )
    }
}

impl std::error::Error for ActorSpawnDiagnostic {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_formats_catalog_values() {
        for (text, sex) in [("male", ActorSex::Male), ("female", ActorSex::Female)] {
            assert_eq!(ActorSex::parse(text), Some(sex));
            assert_eq!(sex.as_str(), text);
        }
        assert_eq!(ActorSex::parse("Male"), None);

        for (text, resolution) in [
            ("body", ActorResolution::Body),
            ("body_head", ActorResolution::BodyHead),
            ("unresolved", ActorResolution::Unresolved),
        ] {
            assert_eq!(ActorResolution::parse(text), Some(resolution));
            assert_eq!(resolution.as_str(), text);
        }
        assert_eq!(ActorResolution::parse("partial"), None);
        assert!(!ActorResolution::Unresolved.is_resolved());
        assert!(ActorResolution::Body.is_resolved());
        assert!(ActorResolution::BodyHead.is_resolved());

        for (text, slot) in [
            ("body", ActorSlot::Body),
            ("hands", ActorSlot::Hands),
            ("feet", ActorSlot::Feet),
        ] {
            assert_eq!(ActorSlot::parse(text), Some(slot));
            assert_eq!(slot.as_str(), text);
        }
        assert_eq!(ActorSlot::parse("head"), None);

        for (text, source) in [
            ("facegen", ActorHeadSource::Facegen),
            ("default", ActorHeadSource::Default),
        ] {
            assert_eq!(ActorHeadSource::parse(text), Some(source));
            assert_eq!(source.as_str(), text);
        }
        assert_eq!(ActorHeadSource::parse("custom"), None);
    }

    #[test]
    fn actor_records_the_base_not_the_reference() {
        let actor = Actor::new(0x13475, 79686, ActorSex::Male, ActorResolution::BodyHead);
        assert_eq!(actor.base_form_id, 0x13475);
        assert_eq!(actor.race_id, 79686);
        assert_eq!(actor.sex, ActorSex::Male);
        assert_eq!(actor.resolution, ActorResolution::BodyHead);
    }

    #[test]
    fn parts_are_ordered_and_keep_every_slot() {
        let parts = ActorParts::new([
            ActorPart::new(ActorSlot::Feet, "MaleFeet_1.nif").unwrap(),
            ActorPart::new(ActorSlot::Body, "MaleBody_1.NIF").unwrap(),
            ActorPart::new(ActorSlot::Hands, "MaleHands_1.nif").unwrap(),
        ]);
        assert_eq!(parts.len(), 3);
        let slots: Vec<_> = parts.iter().map(|part| part.slot).collect();
        assert_eq!(slots, [ActorSlot::Body, ActorSlot::Hands, ActorSlot::Feet]);
        assert_eq!(
            parts
                .get(ActorSlot::Hands)
                .map(|part| part.model_path.as_str()),
            Some("MaleHands_1.nif")
        );
        assert!(parts.get(ActorSlot::Feet).is_some());
    }

    #[test]
    fn rejects_empty_part_and_head_paths() {
        assert!(ActorPart::new(ActorSlot::Body, "  ").is_none());
        assert!(ActorHead::new("", ActorHeadSource::Default).is_none());
        assert!(
            ActorHead::new(
                "meshes/actors/character/character assets/malehead.nif",
                ActorHeadSource::Default
            )
            .is_some()
        );
    }

    #[test]
    fn composed_body_paths_are_per_sex() {
        assert_eq!(
            composed_body_scene(ActorSex::Male),
            "meshes/actors/character/composed/default/male/body.glb"
        );
        assert_eq!(
            composed_body_scene(ActorSex::Female),
            "meshes/actors/character/composed/default/female/body.glb"
        );
    }

    #[test]
    fn a_skip_diagnostic_names_the_base_form_id_once() {
        let diagnostic = ActorSpawnDiagnostic {
            base_form_id: 0x13475,
            reason: ActorSpawnSkip::Unresolved,
        };
        assert_eq!(
            diagnostic.to_string(),
            "NPC_ 00013475: resolution is unresolved"
        );
        let no_body = ActorSpawnDiagnostic {
            base_form_id: 0x13476,
            reason: ActorSpawnSkip::NoBody,
        };
        assert_eq!(
            no_body.to_string(),
            "NPC_ 00013476: no composed body asset or body part model"
        );
    }
}
