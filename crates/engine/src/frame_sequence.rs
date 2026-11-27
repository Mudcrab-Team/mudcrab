//! The consecutive-frame check of a `--shots` pose (`"sequence"` in a shot).
//!
//! A single screenshot misses a fault that shows in some frames only. The `skyrim` profile
//! flickered that way: the untoned scene replaced one frame in a few dozen. A pose with a
//! sequence is captured for that many consecutive frames, and the run fails when a frame is
//! almost one colour or its mean luma leaves the median of the sequence.

use image::RgbImage;
use std::sync::{Arc, Mutex};

/// A frame whose luma standard deviation is below this is solid, in 8-bit steps.
pub const SOLID_DEVIATION: f64 = 2.0;
/// A frame of a still view whose mean luma differs from the median by more than this flashes, in
/// 8-bit steps.
pub const FLASH_LUMA: f64 = 12.0;
/// The longest sequence a shot may ask for.
pub const MAX_FRAMES: u32 = 3600;

/// The luma of one captured frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameStats {
    /// The position in the sequence, from 1.
    pub frame: u32,
    pub mean: f64,
    pub deviation: f64,
}

impl FrameStats {
    /// Every seventh pixel of `image`.
    pub fn of(frame: u32, image: &RgbImage) -> Self {
        let (mut count, mut sum, mut squares) = (0.0, 0.0, 0.0);
        for pixel in image.pixels().step_by(7) {
            let [red, green, blue] = pixel.0.map(f64::from);
            let luma = 0.299 * red + 0.587 * green + 0.114 * blue;
            count += 1.0;
            sum += luma;
            squares += luma * luma;
        }
        let mean: f64 = if count > 0.0 { sum / count } else { 0.0 };
        let variance = if count > 0.0 {
            squares / count - mean * mean
        } else {
            0.0
        };
        Self {
            frame,
            mean,
            deviation: variance.max(0.0).sqrt(),
        }
    }
}

/// One line for each solid or flashing frame of `frames`.
pub fn faults(frames: &[FrameStats]) -> Vec<String> {
    let mut means: Vec<f64> = frames.iter().map(|stats| stats.mean).collect();
    means.sort_by(f64::total_cmp);
    let Some(&median) = means.get(means.len() / 2) else {
        return vec!["the sequence has no frame".to_owned()];
    };
    frames
        .iter()
        .filter_map(|stats| {
            let FrameStats {
                frame,
                mean,
                deviation,
            } = *stats;
            if deviation < SOLID_DEVIATION {
                Some(format!(
                    "frame {frame} is solid (mean {mean:.1}, deviation {deviation:.2})"
                ))
            } else if (mean - median).abs() > FLASH_LUMA {
                Some(format!(
                    "frame {frame} flashes (mean {mean:.1}, median {median:.1})"
                ))
            } else {
                None
            }
        })
        .collect()
}

/// `frame,mean,deviation`, one line for each frame.
pub fn csv(frames: &[FrameStats]) -> String {
    let mut text = "frame,mean_luma,deviation\n".to_owned();
    for stats in frames {
        text += &format!("{},{:.3},{:.3}\n", stats.frame, stats.mean, stats.deviation);
    }
    text
}

/// The frames of the running sequence. The capture observers fill it in any order.
#[derive(Clone, Debug, Default)]
pub struct Sequence {
    pub asked: u32,
    pub received: Arc<Mutex<Vec<Result<FrameStats, String>>>>,
}

impl Sequence {
    /// The frames in order once `wanted` of them have arrived, or the first capture error.
    pub fn finished(&self, wanted: u32) -> Option<Result<Vec<FrameStats>, String>> {
        let Ok(received) = self.received.lock() else {
            return Some(Err("a capture observer panicked".to_owned()));
        };
        if u32::try_from(received.len()).is_ok_and(|arrived| arrived < wanted) {
            return None;
        }
        let mut frames = Vec::with_capacity(received.len());
        for frame in received.iter() {
            match frame {
                Ok(stats) => frames.push(*stats),
                Err(reason) => return Some(Err(reason.clone())),
            }
        }
        frames.sort_by_key(|stats| stats.frame);
        Some(Ok(frames))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(frame: u32, mean: f64, deviation: f64) -> FrameStats {
        FrameStats {
            frame,
            mean,
            deviation,
        }
    }

    /// The measured flicker: single frames of the untoned scene among steady ones.
    #[test]
    fn a_flash_and_a_solid_frame_fail_and_a_slow_drift_passes() {
        let mut frames: Vec<_> = (1..=9)
            .map(|index| frame(index, 110.0 + f64::from(index) * 0.5, 28.5))
            .collect();
        assert_eq!(faults(&frames), Vec::<String>::new());
        frames[3] = frame(4, 173.2, 16.1);
        frames[6] = frame(7, 112.0, 0.4);
        let found = faults(&frames);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].starts_with("frame 4 flashes"), "{found:?}");
        assert!(found[1].starts_with("frame 7 is solid"), "{found:?}");
        assert_eq!(faults(&[]).len(), 1);
    }

    #[test]
    fn the_statistics_are_the_luma_mean_and_deviation() {
        let solid = RgbImage::from_pixel(16, 16, image::Rgb([10, 20, 30]));
        let stats = FrameStats::of(1, &solid);
        assert!((stats.mean - 18.15).abs() < 1.0e-9, "{}", stats.mean);
        assert!(stats.deviation < 1.0e-3);
        let split = RgbImage::from_fn(70, 10, |x, _| {
            image::Rgb([if x % 2 == 0 { 0 } else { 200 }; 3])
        });
        assert!(FrameStats::of(1, &split).deviation > 90.0);
    }

    #[test]
    fn a_sequence_finishes_in_frame_order_and_keeps_a_capture_error() {
        let sequence = Sequence::default();
        assert!(sequence.finished(2).is_none());
        let mut received = sequence.received.lock().unwrap();
        received.push(Ok(frame(2, 1.0, 3.0)));
        received.push(Ok(frame(1, 1.0, 3.0)));
        drop(received);
        let frames = sequence.finished(2).unwrap().unwrap();
        assert_eq!(frames.iter().map(|f| f.frame).collect::<Vec<_>>(), [1, 2]);
        assert!(csv(&frames).starts_with("frame,mean_luma,deviation\n1,1.000,3.000\n"));
        sequence
            .received
            .lock()
            .unwrap()
            .push(Err("lost".to_owned()));
        assert_eq!(sequence.finished(3), Some(Err("lost".to_owned())));
    }
}
