//! Exact Creation-space route expansion. No elapsed time participates in movement.
use crate::shots::{Shot, ShotsError, ShotsFile};
use serde::Deserialize;
use std::{collections::BTreeSet, fs, path::Path};

#[derive(Debug, Deserialize)]
pub(crate) struct MatchedRoute {
    schema_version: u32,
    worldspace_id: u32,
    start_creation: [i32; 3],
    yaw_degrees: f32,
    pitch_degrees: f32,
    hfov_degrees: f32,
    cell_size: i32,
    step_creation_units: i32,
    legs_relative_x: Vec<i32>,
    cycles: usize,
    movement_steps: usize,
}

impl MatchedRoute {
    pub(crate) fn load(path: &Path) -> Result<Self, ShotsError> {
        let text = fs::read_to_string(path).map_err(|e| ShotsError(e.to_string()))?;
        Self::parse(&text)
    }

    #[cfg(test)]
    pub(crate) fn parse_for_test(text: &str) -> (ShotsFile, BTreeSet<usize>) {
        Self::parse(text).unwrap().expand().unwrap()
    }

    fn parse(text: &str) -> Result<Self, ShotsError> {
        let route: Self = serde_json::from_str(text).map_err(|e| ShotsError(e.to_string()))?;
        route.expand()?;
        Ok(route)
    }

    /// Leg values are destinations relative to the start, not displacements.
    pub(crate) fn expand(&self) -> Result<(ShotsFile, BTreeSet<usize>), ShotsError> {
        let invalid = || {
            ShotsError("invalid matched route: schema, bounds, step count or closed legs".into())
        };
        if self.schema_version != 1
            || self.cell_size != 4096
            || self.step_creation_units <= 0
            || self.step_creation_units > self.cell_size
            || self.cycles == 0
            || self.movement_steps > 100_000
            || self.legs_relative_x.last() != Some(&0)
        {
            return Err(invalid());
        }
        let mut relative = vec![0i32];
        let mut checkpoints = BTreeSet::from([0]);
        for _ in 0..self.cycles {
            for &destination in &self.legs_relative_x {
                let from = *relative.last().unwrap();
                let distance = i64::from(destination) - i64::from(from);
                if distance == 0 || distance.abs() % i64::from(self.step_creation_units) != 0 {
                    return Err(invalid());
                }
                let steps = distance.abs() / i64::from(self.step_creation_units);
                if steps as usize > self.movement_steps.saturating_sub(relative.len() - 1) {
                    return Err(invalid());
                }
                let delta = distance.signum() * i64::from(self.step_creation_units);
                for _ in 0..steps {
                    let previous = i64::from(*relative.last().unwrap()) + i64::from(self.start_creation[0]);
                    let next_relative = i64::from(*relative.last().unwrap()) + delta;
                    let next = next_relative + i64::from(self.start_creation[0]);
                    // f32 integer poses must remain exact, including the coordinate conversion.
                    if next.abs() > 16_777_216 || next_relative.abs() > i64::from(i32::MAX) {
                        return Err(invalid());
                    }
                    let index = relative.len();
                    if previous.div_euclid(i64::from(self.cell_size)) != next.div_euclid(i64::from(self.cell_size)) {
                        checkpoints.extend([index - 1, index]);
                    }
                    relative.push(next_relative as i32);
                }
                checkpoints.insert(relative.len() - 1);
            }
        }
        if relative.len() - 1 != self.movement_steps
            || self
                .start_creation
                .iter()
                .any(|v| i64::from(*v).abs() > 16_777_216)
        {
            return Err(invalid());
        }
        let shots = relative.into_iter().enumerate().map(|(step, x)| Shot {
            name: format!("route-{step:06}"),
            worldspace_id: Some(self.worldspace_id),
            interior_cell_id: None,
            position: [(i64::from(self.start_creation[0]) + i64::from(x)) as f32, self.start_creation[1] as f32, self.start_creation[2] as f32],
            yaw: self.yaw_degrees,
            pitch: self.pitch_degrees,
            hfov: self.hfov_degrees,
            reference: None,
            note: None,
        }).collect();
        let file = ShotsFile {
            width: 1600,
            height: 900,
            shots,
        };
        file.validate()?;
        Ok((file, checkpoints))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const FIXTURE: &str = include_str!("../../../scripts/profiling/fixtures/matched-route.json");
    #[test]
    fn exact_fixture_traversal_and_returns() {
        let (file, checkpoints) = MatchedRoute::parse(FIXTURE).unwrap().expand().unwrap();
        assert_eq!(file.shots.len(), 3073);
        for pair in file.shots.windows(2) {
            assert_eq!((pair[1].position[0] - pair[0].position[0]).abs(), 64.0);
            assert_eq!(&pair[1].position[1..], &pair[0].position[1..]);
        }
        for cycle in 0..3 {
            let base = cycle * 1024;
            assert_eq!(file.shots[base + 256].position[0], 38912.0);
            assert_eq!(file.shots[base + 768].position[0], 6144.0);
            assert_eq!(file.shots[base + 1024].position, file.shots[0].position);
            for step in [base + 256, base + 768, base + 1024] { assert!(checkpoints.contains(&step)); }
        }
        assert!(checkpoints.contains(&31));
        assert!(checkpoints.contains(&32));
    }
    #[test]
    fn refuses_inconsistent_or_unbounded_routes() {
        for text in [FIXTURE.replace("3072", "3071"), FIXTURE.replace("3072", "100001"), FIXTURE.replace("\"step_creation_units\": 64", "\"step_creation_units\": 63"), FIXTURE.replace("\"cycles\": 3", "\"cycles\": 0")] {
            assert!(MatchedRoute::parse(&text).is_err());
        }
    }
}
