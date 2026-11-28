//! Frame image-space modifiers, from SPEC-2 section 2.3.

use crate::retail_hdr::HdrBlock;
use bevy::prelude::*;

/// A sampled modifier instance. The producer resolves the instance's animation curves.
/// The engine does not yet decode or sample IMAD animation curves or effect flags.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ImageSpaceModifier {
    pub weight: f32,
    /// In HNAM then CNAM order.
    pub multiply: [f32; 12],
    pub add: [f32; 12],
    pub tint: WeightedColour,
    pub fade: WeightedColour,
    pub blur_radius: f32,
    pub double_vision: f32,
    pub radial_blur: [f32; 6],
    pub depth_of_field: [f32; 4],
    pub extra_strength: f32,
}

impl Default for ImageSpaceModifier {
    fn default() -> Self {
        Self {
            weight: 0.0,
            multiply: [1.0; 12],
            add: [0.0; 12],
            tint: WeightedColour::default(),
            fade: WeightedColour::default(),
            blur_radius: 0.0,
            double_vision: 0.0,
            radial_blur: [0.0; 6],
            depth_of_field: [0.0; 4],
            extra_strength: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct WeightedColour {
    pub amount: f32,
    pub rgb: Vec3,
}

#[derive(Default)]
struct ColourAccumulator {
    maximum: f32,
    sum: f32,
    rgb: Vec3,
}

impl ColourAccumulator {
    fn add(&mut self, colour: WeightedColour) {
        self.maximum = self.maximum.max(colour.amount);
        self.sum += colour.amount;
        self.rgb += colour.amount * colour.rgb;
    }

    fn finish(self) -> WeightedColour {
        if self.sum > 0.0 {
            WeightedColour {
                amount: self.maximum,
                rgb: self.rgb / self.sum,
            }
        } else {
            WeightedColour {
                amount: self.maximum,
                rgb: self.rgb,
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ImageSpaceEffects {
    pub fade: WeightedColour,
    pub blur_radius: f32,
    pub double_vision: f32,
    pub radial_blur: [f32; 6],
    pub depth_of_field: [f32; 4],
    pub extra_strength: f32,
}

#[derive(Resource, Default)]
pub(crate) struct RetailImageSpaceInputs {
    pub override_block: Option<HdrBlock>,
    pub modifiers: Vec<ImageSpaceModifier>,
    pub map_open: bool,
    pub hdr_disabled: bool,
    /// Change this generation to discard every view's adapted pair.
    pub adaptation_generation: u32,
    /// The previous weather and the current weather's share during a transition.
    pub weather_transition: Option<(crate::weather_sky::WeatherSky, f32)>,
}

#[derive(Default)]
pub(crate) struct RoomImageSpace {
    cell: Option<u32>,
    previous: Option<u32>,
    current: Option<u32>,
    elapsed: f32,
}

impl RoomImageSpace {
    pub(crate) fn forms(
        &mut self,
        cell: Option<u32>,
        room: Option<u32>,
        seconds: f32,
    ) -> [(Option<u32>, f32); 2] {
        if self.cell != cell {
            self.cell = cell;
            self.previous = None;
            self.current = room;
            self.elapsed = 3.0;
        } else if self.current != room {
            self.previous = self.current;
            self.current = room;
            self.elapsed = 0.0;
        }
        // SPEC-2 §2.2 row 6 advances on each update. The reset order is open. Advance
        // after a room reset so the new room does not flash before the fade starts.
        self.elapsed = (self.elapsed + seconds).min(3.0);
        let s = self.elapsed / 3.0;
        let m = s * s * (3.0 - 2.0 * s);
        if m == 0.0 {
            return [(self.previous, 0.0), (self.current, 1.0)];
        }
        [(self.previous, 1.0 - m), (self.current, m)]
    }
}

/// Apply each instance in order. Tint and fade contributions carry independent amounts.
pub(crate) fn process_frame(
    mut block: HdrBlock,
    modifiers: &[ImageSpaceModifier],
) -> (HdrBlock, ImageSpaceEffects) {
    let base_depth_of_field = block.depth_of_field;
    let mut effects = ImageSpaceEffects::default();
    let mut tint = ColourAccumulator {
        maximum: block.tint_amount,
        sum: block.tint_amount,
        rgb: block.tint_amount * Vec3::from_array(block.tint_rgb),
    };
    let mut fade = ColourAccumulator::default();
    for modifier in modifiers {
        let fields = [
            &mut block.eye_adapt_speed,
            &mut block.bloom_blur_radius,
            &mut block.bloom_threshold,
            &mut block.bloom_scale,
            &mut block.receive_bloom_threshold,
            &mut block.white,
            &mut block.sunlight_scale,
            &mut block.sky_scale,
            &mut block.eye_adapt_strength,
            &mut block.saturation,
            &mut block.brightness,
            &mut block.contrast,
        ];
        for ((value, multiply), add) in fields.into_iter().zip(modifier.multiply).zip(modifier.add)
        {
            *value -= modifier.weight * (*value - (*value * multiply + add));
        }
        tint.add(modifier.tint);
        fade.add(modifier.fade);
        effects.blur_radius = effects.blur_radius.max(modifier.blur_radius);
        effects.double_vision = effects.double_vision.max(modifier.double_vision);
        effects.extra_strength = effects.extra_strength.max(modifier.extra_strength);
        if modifier.radial_blur[0] > effects.radial_blur[0] {
            effects.radial_blur = modifier.radial_blur;
        }
        if modifier.depth_of_field[0] > effects.depth_of_field[0] {
            effects.depth_of_field = modifier.depth_of_field;
        }
    }
    let tint = tint.finish();
    block.tint_amount = tint.amount;
    block.tint_rgb = tint.rgb.to_array();
    effects.fade = fade.finish();
    if effects.depth_of_field[0] == 0.0 {
        effects.depth_of_field = base_depth_of_field;
    }
    (block, effects)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paused_room_change_uses_the_current_block_at_zero_weight() {
        let mut state = RoomImageSpace::default();
        state.forms(Some(1), Some(2), 0.1);
        assert_eq!(
            state.forms(Some(1), Some(3), 0.0),
            [(Some(2), 0.0), (Some(3), 1.0)]
        );
    }

    #[test]
    fn room_fade_starts_on_the_change_frame_and_resets_on_cell_change() {
        let mut state = RoomImageSpace::default();
        assert_eq!(
            state.forms(Some(1), Some(2), 0.1),
            [(None, 0.0), (Some(2), 1.0)]
        );
        let [(previous, old_share), (current, new_share)] = state.forms(Some(1), Some(3), 0.1);
        assert_eq!((previous, current), (Some(2), Some(3)));
        assert!(old_share > 0.99 && new_share > 0.0);
        let [(_, old_share), (_, new_share)] = state.forms(Some(1), Some(3), 0.0);
        assert!(old_share > 0.99 && new_share > 0.0);
        assert_eq!(
            state.forms(Some(1), Some(3), 3.0),
            [(Some(2), 0.0), (Some(3), 1.0)]
        );
        assert_eq!(state.forms(Some(4), None, 0.1), [(None, 0.0), (None, 1.0)]);
    }
}
