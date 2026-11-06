//! Character motion in native Havok units, sampled before collision resolution.

use color_eyre::{Result, eyre::ensure};

const HAVOK_TO_CREATION: f32 = f32::from_bits(0x428b_fb85);
const CREATION_TO_HAVOK: f32 = f32::from_bits(0x3c6a_161e);
const GRAVITY_HAVOK: f32 = f32::from_bits(0x411c_f5c3);
const FALLING_HISTORY_MULTIPLIER: f32 = 1.35;

/// Finite signed effective Creation height. Native launch uses its magnitude.
#[derive(Debug, Clone)]
pub struct JumpHeight {
    creation: f32,
    source: String,
}

impl JumpHeight {
    pub fn new(creation: f32, source: String) -> Result<Self> {
        ensure!(creation.is_finite(), "effective jump height is nonfinite");
        ensure!(
            (2.0 * GRAVITY_HAVOK * (creation * CREATION_TO_HAVOK).abs()).is_finite(),
            "effective jump height overflows native impulse arithmetic"
        );
        Ok(Self { creation, source })
    }

    pub fn creation(&self) -> f32 {
        self.creation
    }

    pub fn source(&self) -> &str {
        &self.source
    }
}

/// The preceding collision step's actual body position and support signal.
#[derive(Debug, Clone, Copy)]
pub struct ResolvedWalk {
    pub y: f32,
    pub grounded: bool,
}

#[derive(Debug, Clone, Copy, Default)]
enum Phase {
    #[default]
    Uninitialized,
    Supported {
        y: f32,
    },
    Airborne {
        velocity_havok: f32,
        maximum_y: f32,
        launched: bool,
    },
}

#[derive(Debug, Clone, Copy)]
struct JumpObservation {
    origin_y: f32,
    apex: f32,
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct VerticalMotion {
    phase: Phase,
    observation: Option<JumpObservation>,
}

impl VerticalMotion {
    pub(super) fn advance(
        &mut self,
        height: &JumpHeight,
        resolved: ResolvedWalk,
        jump: bool,
        dt: f32,
    ) {
        if self.jump_active()
            && let Some(observation) = &mut self.observation
        {
            observation.apex = observation.apex.max(resolved.y - observation.origin_y);
        }

        // Rapier can report proximity support during takeoff. Preserve an
        // upward launched flight until its velocity becomes nonpositive.
        let supported =
            resolved.grounded && !(self.jump_active() && self.velocity_creation() > 0.0);
        if supported {
            self.phase = Phase::Supported { y: resolved.y };
            if !jump {
                return;
            }
            self.observation = Some(JumpObservation {
                origin_y: resolved.y,
                apex: 0.0,
            });
            self.phase = Phase::Airborne {
                velocity_havok: (2.0 * GRAVITY_HAVOK * (height.creation * CREATION_TO_HAVOK).abs())
                    .sqrt(),
                maximum_y: resolved.y,
                launched: true,
            };
        } else if !matches!(self.phase, Phase::Airborne { .. }) {
            // Leaving support uses the same history law. This is the engine's
            // falling policy; no new native ledge behavior is inferred.
            let maximum_y = match self.phase {
                Phase::Supported { y } => y,
                _ => resolved.y,
            };
            self.phase = Phase::Airborne {
                velocity_havok: 0.0,
                maximum_y,
                launched: false,
            };
        }

        if let Phase::Airborne {
            velocity_havok,
            maximum_y,
            ..
        } = &mut self.phase
        {
            let multiplier = if resolved.y > *maximum_y {
                *maximum_y = resolved.y;
                1.0
            } else {
                FALLING_HISTORY_MULTIPLIER
            };
            *velocity_havok -= GRAVITY_HAVOK * multiplier * dt;
        }
    }

    pub(super) fn velocity_creation(&self) -> f32 {
        match self.phase {
            Phase::Airborne { velocity_havok, .. } => velocity_havok * HAVOK_TO_CREATION,
            _ => 0.0,
        }
    }

    pub(super) fn leave_support(&mut self) {
        if let Phase::Supported { y } = self.phase {
            self.phase = Phase::Airborne {
                velocity_havok: 0.0,
                maximum_y: y,
                launched: false,
            };
        }
    }

    pub(super) fn grounded(&self) -> bool {
        matches!(self.phase, Phase::Supported { .. })
    }

    pub(super) fn jump_active(&self) -> bool {
        matches!(self.phase, Phase::Airborne { launched: true, .. })
    }

    pub(super) fn jump_apex(&self) -> f32 {
        self.observation.map_or(0.0, |observation| observation.apex)
    }
}
