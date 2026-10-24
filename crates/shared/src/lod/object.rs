//! Conservative object eligibility and visual coverage ownership.
//!
//! Authored, generated conventional, and cluster LOD share these contracts.
//! Detail selection does not own world residency, enable state, collision,
//! scripts, inventory, or other gameplay state. The initial static subset is
//! verified, unconditionally enabled exterior `STAT` references with opaque
//! rigid geometry. An unknown fact is an omission, never an implicit opt-in.

/// Identity of one placement in a validated asset package's resolved load order.
///
/// A reference's position or source cell may change without changing this key.
/// FormIDs are not stable across arbitrary load-order changes; a new package or
/// world context must invalidate the runtime generation before reusing a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectReferenceKey {
    pub worldspace_id: u32,
    pub reference_id: u32,
}

/// SHA-256 identity of shared primitive geometry and its material contract.
///
/// The producer defines and versions the hashed input. Placements sharing this
/// key still have independent [`ObjectReferenceKey`] and handoff state. A cache
/// must also validate its producer/settings identity before reusing payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectGeometryKey(pub [u8; 32]);

/// Classification from the authoritative base record, not the `statics` catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectBaseKind {
    /// The authoritative record type is exactly `STAT`.
    Stat,
    /// A type requiring gameplay, movement, or independently managed state.
    Dynamic,
    Unsupported,
    Unverified,
}

/// Enable metadata alone does not prove that scripts cannot change a reference.
/// The caller must also verify [`ObjectBehavior::Fixed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectEnableState {
    Unconditional,
    InitiallyDisabled,
    /// Preserve the original parent and inversion flags for diagnostics. Both
    /// inversion modes, cleared parents, broken parents, and cycles are omitted
    /// from the static path; this contract does not evaluate an enable graph.
    EnableParent {
        parent_id: u32,
        flags: u32,
    },
    Unverified,
}

impl ObjectEnableState {
    /// Classify exported reference metadata without inventing absent facts.
    ///
    /// `header_flags = None` means an older or unverified metadata producer.
    /// `enable_metadata_verified` must establish that absent parent columns
    /// mean an absent winning XESP, rather than a malformed/discarded subrecord.
    /// A partial parent/flags pair is also unverified. Presence of an XESP pair
    /// is excluded even if its remapped parent is zero.
    pub fn from_metadata(
        header_flags: Option<u32>,
        enable_parent_id: Option<u32>,
        enable_parent_flags: Option<u32>,
        enable_metadata_verified: bool,
    ) -> Self {
        let Some(header_flags) = header_flags else {
            return Self::Unverified;
        };
        // REFR's record-header Initially Disabled flag, not a subrecord flag.
        if header_flags & 0x0000_0800 != 0 {
            return Self::InitiallyDisabled;
        }
        if !enable_metadata_verified {
            return Self::Unverified;
        }
        match (enable_parent_id, enable_parent_flags) {
            (None, None) => Self::Unconditional,
            (Some(parent_id), Some(flags)) => Self::EnableParent { parent_id, flags },
            _ => Self::Unverified,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectBehavior {
    /// Verified stationary geometry with no mutable state requiring a proxy.
    Fixed,
    Movable,
    Animated,
    Unverified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LargeReferenceState {
    /// Verified not subject to separate large-reference coverage.
    NotLarge,
    /// A validated policy establishes exclusive coverage and handoff ownership.
    VerifiedOwnership,
    /// Includes unknown policy or unresolved overlap with another representation.
    Unverified,
}

/// Reference facts required before baking any of its primitives into static LOD.
///
/// No defaults are provided: absent metadata or behavior evidence cannot become
/// a stationary, unconditionally enabled object by omission. Primitive
/// suitability must also pass [`PrimitiveEligibility::evaluate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReferenceEligibility {
    pub base_kind: ObjectBaseKind,
    pub is_exterior: bool,
    pub enable_state: ObjectEnableState,
    pub behavior: ObjectBehavior,
    /// Finite transform, usable scale, and finite ordered conservative bounds
    /// were checked against the winning source and its transformed geometry.
    pub placement_verified: bool,
    pub large_reference: LargeReferenceState,
}

impl ReferenceEligibility {
    /// Return the first omission in a stable diagnostic order.
    pub fn evaluate(self) -> Result<(), ObjectLodOmission> {
        if !self.is_exterior {
            return Err(ObjectLodOmission::NonExterior);
        }
        match self.base_kind {
            ObjectBaseKind::Stat => {}
            ObjectBaseKind::Dynamic => return Err(ObjectLodOmission::DynamicBase),
            ObjectBaseKind::Unsupported => return Err(ObjectLodOmission::UnsupportedBase),
            ObjectBaseKind::Unverified => return Err(ObjectLodOmission::UnverifiedBase),
        }
        match self.enable_state {
            ObjectEnableState::Unconditional => {}
            ObjectEnableState::InitiallyDisabled => {
                return Err(ObjectLodOmission::InitiallyDisabled);
            }
            ObjectEnableState::EnableParent { .. } => {
                return Err(ObjectLodOmission::EnableDependent);
            }
            ObjectEnableState::Unverified => return Err(ObjectLodOmission::UnverifiedEnableState),
        }
        match self.behavior {
            ObjectBehavior::Fixed => {}
            ObjectBehavior::Movable => return Err(ObjectLodOmission::Movable),
            ObjectBehavior::Animated => return Err(ObjectLodOmission::Animated),
            ObjectBehavior::Unverified => return Err(ObjectLodOmission::UnverifiedBehavior),
        }
        if !self.placement_verified {
            return Err(ObjectLodOmission::UnverifiedPlacement);
        }
        if self.large_reference == LargeReferenceState::Unverified {
            return Err(ObjectLodOmission::UnverifiedLargeReference);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimitiveMaterial {
    Opaque,
    Cutout,
    Blend,
    Unsupported,
    Unverified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimitiveDeformation {
    Rigid,
    Skinned,
    Animated,
    Unverified,
}

/// Representation-independent primitive suitability for the initial experiment.
///
/// Evaluate each material primitive separately. An opaque primitive's result
/// cannot certify a sibling cutout or blended primitive. Small eligible meshes
/// may keep ordinary source geometry; size thresholds are a detail policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrimitiveEligibility {
    pub material: PrimitiveMaterial,
    pub deformation: PrimitiveDeformation,
    /// Topology, indices, positions, and required attributes were validated.
    pub geometry_verified: bool,
}

impl PrimitiveEligibility {
    pub fn evaluate(self) -> Result<(), ObjectLodOmission> {
        match self.deformation {
            PrimitiveDeformation::Rigid => {}
            PrimitiveDeformation::Skinned => return Err(ObjectLodOmission::Skinned),
            PrimitiveDeformation::Animated => return Err(ObjectLodOmission::Animated),
            PrimitiveDeformation::Unverified => {
                return Err(ObjectLodOmission::UnverifiedDeformation);
            }
        }
        match self.material {
            PrimitiveMaterial::Opaque => {}
            PrimitiveMaterial::Cutout => return Err(ObjectLodOmission::Cutout),
            PrimitiveMaterial::Blend => return Err(ObjectLodOmission::Blended),
            PrimitiveMaterial::Unsupported => return Err(ObjectLodOmission::UnsupportedMaterial),
            PrimitiveMaterial::Unverified => return Err(ObjectLodOmission::UnverifiedMaterial),
        }
        if !self.geometry_verified {
            return Err(ObjectLodOmission::UnverifiedGeometry);
        }
        Ok(())
    }
}

/// Explicit omissions shared by converter reports and future runtime adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectLodOmission {
    NonExterior,
    DynamicBase,
    UnsupportedBase,
    UnverifiedBase,
    InitiallyDisabled,
    EnableDependent,
    UnverifiedEnableState,
    Movable,
    Animated,
    UnverifiedBehavior,
    UnverifiedPlacement,
    UnverifiedLargeReference,
    Skinned,
    UnverifiedDeformation,
    Cutout,
    Blended,
    UnsupportedMaterial,
    UnverifiedMaterial,
    UnverifiedGeometry,
}

/// Whole-object visual alternatives. A cluster representation must cover every
/// required primitive with a complete drawable cut; individual clusters or
/// partially ready source primitives cannot retire the object's fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectRepresentation {
    Source,
    AuthoredLod,
    ConventionalLod,
    ClusterLod,
}

impl ObjectRepresentation {
    const ALL: [Self; 4] = [
        Self::Source,
        Self::AuthoredLod,
        Self::ConventionalLod,
        Self::ClusterLod,
    ];

    const fn index(self) -> usize {
        match self {
            Self::Source => 0,
            Self::AuthoredLod => 1,
            Self::ConventionalLod => 2,
            Self::ClusterLod => 3,
        }
    }
}

/// Runtime context and reference revision. Increment the context on world/load
/// changes and the revision when source state or geometry changes. Epochs must
/// not wrap or be reused during a handoff owner's lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectGeneration {
    pub context_epoch: u64,
    pub reference_revision: u64,
}

/// Stamp every asynchronous completion with its placement and generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectReadinessToken {
    pub reference: ObjectReferenceKey,
    pub generation: ObjectGeneration,
    pub representation: ObjectRepresentation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectReadiness {
    Unavailable,
    Pending,
    /// The complete replacement has renderable geometry, materials, textures,
    /// and current transforms. Asset loading alone does not establish this.
    Drawable,
    Failed,
}

/// Exactly one visual owner or an explicit lack of drawable coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectCoverage {
    Missing,
    Drawable(ObjectRepresentation),
}

/// Apply the previous/current visibility change together before drawing. This
/// changes rendering only; the full runtime remains the sole gameplay owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectCoverageChange {
    pub previous: ObjectCoverage,
    pub current: ObjectCoverage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectHandoffError {
    WrongReference,
    StaleGeneration,
    GenerationNotNewer,
}

/// Per-reference visual handoff, independent of representation implementation.
///
/// A pending or failed preferred replacement keeps the current drawable owner.
/// If that owner disappears, another drawable alternative supplies fallback.
/// No alternative is fabricated when none is drawable. Generation invalidation
/// clears all coverage rather than drawing an object from an old world/state.
/// This does not create engine entities or enforce renderer visibility itself.
#[derive(Debug, Clone)]
pub struct ObjectHandoff {
    reference: ObjectReferenceKey,
    generation: ObjectGeneration,
    preferred: ObjectRepresentation,
    readiness: [ObjectReadiness; 4],
    coverage: ObjectCoverage,
}

impl ObjectHandoff {
    pub fn new(reference: ObjectReferenceKey, generation: ObjectGeneration) -> Self {
        Self {
            reference,
            generation,
            preferred: ObjectRepresentation::Source,
            readiness: [ObjectReadiness::Unavailable; 4],
            coverage: ObjectCoverage::Missing,
        }
    }

    pub fn coverage(&self) -> ObjectCoverage {
        self.coverage
    }

    pub fn token(&self, representation: ObjectRepresentation) -> ObjectReadinessToken {
        ObjectReadinessToken {
            reference: self.reference,
            generation: self.generation,
            representation,
        }
    }

    pub fn prefer(&mut self, representation: ObjectRepresentation) -> ObjectCoverageChange {
        self.preferred = representation;
        self.reselect()
    }

    /// Accept readiness only for this placement's exact current generation.
    pub fn update_readiness(
        &mut self,
        token: ObjectReadinessToken,
        readiness: ObjectReadiness,
    ) -> Result<ObjectCoverageChange, ObjectHandoffError> {
        if token.reference != self.reference {
            return Err(ObjectHandoffError::WrongReference);
        }
        if token.generation != self.generation {
            return Err(ObjectHandoffError::StaleGeneration);
        }
        self.readiness[token.representation.index()] = readiness;
        Ok(self.reselect())
    }

    /// Clear stale visual ownership when world context or reference state changes.
    pub fn invalidate(
        &mut self,
        generation: ObjectGeneration,
    ) -> Result<ObjectCoverageChange, ObjectHandoffError> {
        if generation <= self.generation {
            return Err(ObjectHandoffError::GenerationNotNewer);
        }
        self.generation = generation;
        self.readiness = [ObjectReadiness::Unavailable; 4];
        Ok(self.reselect())
    }

    fn is_drawable(&self, representation: ObjectRepresentation) -> bool {
        self.readiness[representation.index()] == ObjectReadiness::Drawable
    }

    fn reselect(&mut self) -> ObjectCoverageChange {
        let previous = self.coverage;
        let owner = if self.is_drawable(self.preferred) {
            Some(self.preferred)
        } else if let ObjectCoverage::Drawable(current) = previous {
            self.is_drawable(current).then_some(current)
        } else {
            None
        };
        let owner = owner.or_else(|| {
            ObjectRepresentation::ALL
                .into_iter()
                .find(|&representation| self.is_drawable(representation))
        });
        self.coverage = owner.map_or(ObjectCoverage::Missing, ObjectCoverage::Drawable);
        ObjectCoverageChange {
            previous,
            current: self.coverage,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_reference() -> ReferenceEligibility {
        ReferenceEligibility {
            base_kind: ObjectBaseKind::Stat,
            is_exterior: true,
            enable_state: ObjectEnableState::from_metadata(Some(0), None, None, true),
            behavior: ObjectBehavior::Fixed,
            placement_verified: true,
            large_reference: LargeReferenceState::NotLarge,
        }
    }

    fn opaque_primitive() -> PrimitiveEligibility {
        PrimitiveEligibility {
            material: PrimitiveMaterial::Opaque,
            deformation: PrimitiveDeformation::Rigid,
            geometry_verified: true,
        }
    }

    fn handoff() -> ObjectHandoff {
        ObjectHandoff::new(
            ObjectReferenceKey {
                worldspace_id: 0x3c,
                reference_id: 0x1234,
            },
            ObjectGeneration {
                context_epoch: 7,
                reference_revision: 2,
            },
        )
    }

    #[test]
    fn enable_metadata_never_certifies_unknown_or_xesp_controlled_references() {
        let mut reference = fixed_reference();
        assert_eq!(reference.evaluate(), Ok(()));
        reference.enable_state = ObjectEnableState::from_metadata(None, None, None, true);
        assert_eq!(
            reference.evaluate(),
            Err(ObjectLodOmission::UnverifiedEnableState)
        );
        reference.enable_state = ObjectEnableState::from_metadata(Some(0), None, None, false);
        assert_eq!(
            reference.evaluate(),
            Err(ObjectLodOmission::UnverifiedEnableState)
        );
        reference.enable_state = ObjectEnableState::from_metadata(Some(0x800), None, None, true);
        assert_eq!(
            reference.evaluate(),
            Err(ObjectLodOmission::InitiallyDisabled)
        );
        // Both inversion modes and a cleared/missing parent stay out of static
        // batches. Graph validation belongs to the later individual proxy path.
        for parent in [0, 0x1234] {
            for flags in [0, 1] {
                reference.enable_state =
                    ObjectEnableState::from_metadata(Some(0), Some(parent), Some(flags), true);
                assert_eq!(
                    reference.evaluate(),
                    Err(ObjectLodOmission::EnableDependent)
                );
            }
        }
        reference.enable_state =
            ObjectEnableState::from_metadata(Some(0), Some(0x1234), None, true);
        assert_eq!(
            reference.evaluate(),
            Err(ObjectLodOmission::UnverifiedEnableState)
        );
    }

    #[test]
    fn catalog_membership_cannot_admit_dynamic_or_unverified_placements() {
        let eligible = fixed_reference();
        for (base_kind, omission) in [
            (ObjectBaseKind::Dynamic, ObjectLodOmission::DynamicBase),
            (
                ObjectBaseKind::Unsupported,
                ObjectLodOmission::UnsupportedBase,
            ),
            (
                ObjectBaseKind::Unverified,
                ObjectLodOmission::UnverifiedBase,
            ),
        ] {
            assert_eq!(
                ReferenceEligibility {
                    base_kind,
                    ..eligible
                }
                .evaluate(),
                Err(omission)
            );
        }
        for (behavior, omission) in [
            (ObjectBehavior::Movable, ObjectLodOmission::Movable),
            (ObjectBehavior::Animated, ObjectLodOmission::Animated),
            (
                ObjectBehavior::Unverified,
                ObjectLodOmission::UnverifiedBehavior,
            ),
        ] {
            assert_eq!(
                ReferenceEligibility {
                    behavior,
                    ..eligible
                }
                .evaluate(),
                Err(omission)
            );
        }
        assert_eq!(
            ReferenceEligibility {
                is_exterior: false,
                ..eligible
            }
            .evaluate(),
            Err(ObjectLodOmission::NonExterior)
        );
        assert_eq!(
            ReferenceEligibility {
                placement_verified: false,
                ..eligible
            }
            .evaluate(),
            Err(ObjectLodOmission::UnverifiedPlacement)
        );
        assert_eq!(
            ReferenceEligibility {
                large_reference: LargeReferenceState::Unverified,
                ..eligible
            }
            .evaluate(),
            Err(ObjectLodOmission::UnverifiedLargeReference)
        );
    }

    #[test]
    fn opaque_siblings_do_not_certify_cutout_blended_or_deformed_primitives() {
        let eligible = opaque_primitive();
        assert_eq!(eligible.evaluate(), Ok(()));
        for (material, omission) in [
            (PrimitiveMaterial::Cutout, ObjectLodOmission::Cutout),
            (PrimitiveMaterial::Blend, ObjectLodOmission::Blended),
            (
                PrimitiveMaterial::Unsupported,
                ObjectLodOmission::UnsupportedMaterial,
            ),
            (
                PrimitiveMaterial::Unverified,
                ObjectLodOmission::UnverifiedMaterial,
            ),
        ] {
            assert_eq!(
                PrimitiveEligibility {
                    material,
                    ..eligible
                }
                .evaluate(),
                Err(omission)
            );
        }
        for (deformation, omission) in [
            (PrimitiveDeformation::Skinned, ObjectLodOmission::Skinned),
            (PrimitiveDeformation::Animated, ObjectLodOmission::Animated),
            (
                PrimitiveDeformation::Unverified,
                ObjectLodOmission::UnverifiedDeformation,
            ),
        ] {
            assert_eq!(
                PrimitiveEligibility {
                    deformation,
                    ..eligible
                }
                .evaluate(),
                Err(omission)
            );
        }
        assert_eq!(
            PrimitiveEligibility {
                geometry_verified: false,
                ..eligible
            }
            .evaluate(),
            Err(ObjectLodOmission::UnverifiedGeometry)
        );
    }

    #[test]
    fn delayed_and_failed_near_geometry_retains_far_coverage_until_drawable() {
        let mut owner = handoff();
        let far = owner.token(ObjectRepresentation::AuthoredLod);
        let near = owner.token(ObjectRepresentation::Source);
        owner
            .update_readiness(far, ObjectReadiness::Drawable)
            .unwrap();
        for readiness in [ObjectReadiness::Pending, ObjectReadiness::Failed] {
            let change = owner.update_readiness(near, readiness).unwrap();
            assert_eq!(
                change.current,
                ObjectCoverage::Drawable(ObjectRepresentation::AuthoredLod)
            );
            assert_eq!(change.current, change.previous);
        }
        let change = owner
            .update_readiness(near, ObjectReadiness::Drawable)
            .unwrap();
        assert_eq!(
            change.previous,
            ObjectCoverage::Drawable(ObjectRepresentation::AuthoredLod)
        );
        assert_eq!(
            change.current,
            ObjectCoverage::Drawable(ObjectRepresentation::Source)
        );
        let change = owner
            .update_readiness(near, ObjectReadiness::Unavailable)
            .unwrap();
        assert_eq!(
            change.current,
            ObjectCoverage::Drawable(ObjectRepresentation::AuthoredLod)
        );
    }

    #[test]
    fn automatic_paths_share_exclusive_drawable_handoff_and_source_fallback() {
        let mut owner = handoff();
        let source = owner.token(ObjectRepresentation::Source);
        let conventional = owner.token(ObjectRepresentation::ConventionalLod);
        let cluster = owner.token(ObjectRepresentation::ClusterLod);
        owner
            .update_readiness(source, ObjectReadiness::Drawable)
            .unwrap();
        owner.prefer(ObjectRepresentation::ConventionalLod);
        assert_eq!(
            owner.coverage(),
            ObjectCoverage::Drawable(ObjectRepresentation::Source)
        );
        owner
            .update_readiness(conventional, ObjectReadiness::Drawable)
            .unwrap();
        owner.prefer(ObjectRepresentation::ClusterLod);
        assert_eq!(
            owner.coverage(),
            ObjectCoverage::Drawable(ObjectRepresentation::ConventionalLod)
        );
        owner
            .update_readiness(cluster, ObjectReadiness::Drawable)
            .unwrap();
        assert_eq!(
            owner.coverage(),
            ObjectCoverage::Drawable(ObjectRepresentation::ClusterLod)
        );
        owner
            .update_readiness(cluster, ObjectReadiness::Failed)
            .unwrap();
        assert_eq!(
            owner.coverage(),
            ObjectCoverage::Drawable(ObjectRepresentation::Source)
        );
        owner
            .update_readiness(source, ObjectReadiness::Unavailable)
            .unwrap();
        owner
            .update_readiness(conventional, ObjectReadiness::Unavailable)
            .unwrap();
        assert_eq!(owner.coverage(), ObjectCoverage::Missing);
    }

    #[test]
    fn old_world_completions_cannot_restore_invalidated_geometry() {
        let mut owner = handoff();
        let old = owner.token(ObjectRepresentation::Source);
        owner
            .update_readiness(old, ObjectReadiness::Drawable)
            .unwrap();
        // A newer context may restart the per-reference revision at zero.
        owner
            .invalidate(ObjectGeneration {
                context_epoch: 8,
                reference_revision: 0,
            })
            .unwrap();
        assert_eq!(owner.coverage(), ObjectCoverage::Missing);
        assert_eq!(
            owner.update_readiness(old, ObjectReadiness::Drawable),
            Err(ObjectHandoffError::StaleGeneration)
        );
        let current = owner.token(ObjectRepresentation::Source);
        owner
            .update_readiness(current, ObjectReadiness::Drawable)
            .unwrap();
        assert_eq!(
            owner.invalidate(old.generation),
            Err(ObjectHandoffError::GenerationNotNewer)
        );
        assert_eq!(
            owner.coverage(),
            ObjectCoverage::Drawable(ObjectRepresentation::Source)
        );
    }

    #[test]
    fn shared_geometry_does_not_share_reference_readiness_or_visual_ownership() {
        let geometry = ObjectGeometryKey([0x42; 32]);
        let mut first = handoff();
        let mut second = ObjectHandoff::new(
            ObjectReferenceKey {
                reference_id: 0x5678,
                ..first.reference
            },
            first.generation,
        );
        let first_binding = (first.reference, geometry);
        let second_binding = (second.reference, geometry);
        assert_eq!(first_binding.1, second_binding.1);
        assert_ne!(first_binding.0, second_binding.0);
        let token = first.token(ObjectRepresentation::Source);
        first
            .update_readiness(token, ObjectReadiness::Drawable)
            .unwrap();
        assert_eq!(
            second.update_readiness(token, ObjectReadiness::Drawable),
            Err(ObjectHandoffError::WrongReference)
        );
        assert_eq!(second.coverage(), ObjectCoverage::Missing);
        assert_eq!(
            first.coverage(),
            ObjectCoverage::Drawable(ObjectRepresentation::Source)
        );
    }
}
