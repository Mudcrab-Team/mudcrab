//! The order of every pass that swaps the post-process texture.
//!
//! `ViewTarget::post_process_write` flips the view's main texture while the system runs, and the
//! command buffers are submitted in schedule order. Two such systems without an order between
//! them can run in the other order than they are submitted in, and the frame then shows a texture
//! nobody wrote: the untoned scene. Bevy's ambiguity check cannot see the flip, because both
//! systems take the view target immutably. The `skyrim` profile flickered this way: the retail
//! tonemap and the cinematic grade both ran only after Bevy's tone mapping.

use crate::color_pipeline::SkyrimCinematicPost;
use bevy::{
    core_pipeline::{
        Core3d,
        fullscreen_material::{FullscreenMaterial, fullscreen_material_system},
        tonemapping::tonemapping,
    },
    ecs::schedule::{NodeId, Schedule, ScheduleBuildSettings},
    prelude::*,
};
use std::collections::{HashMap, HashSet};

/// The engine files that swap the post-process texture, or declare a fullscreen material.
const SWAPPING_FILES: [&str; 3] = [
    "color_pipeline.rs",
    "retail_hdr_render.rs",
    "underwater_pass.rs",
];

/// The number of systems of `schedule`, and each pair of them of which the first runs before the
/// second, through any chain of orderings and sets. The orderings are read as configured, before
/// the schedule is built.
fn ordered_pairs(schedule: &mut Schedule) -> (usize, HashSet<(usize, usize)>) {
    let graph = schedule.graph();
    // The systems a node stands for: itself, or every system below a set.
    let members = |node: NodeId| {
        let mut found = Vec::new();
        let mut open = vec![node];
        while let Some(node) = open.pop() {
            match node {
                NodeId::System(key) => found.push(key),
                NodeId::Set(_) => open.extend(graph.hierarchy().graph().neighbors(node)),
            }
        }
        found
    };
    let edges: Vec<_> = graph
        .dependency()
        .graph()
        .all_edges()
        .flat_map(|(before, later)| {
            let later = members(later);
            members(before)
                .into_iter()
                .flat_map(move |from| later.clone().into_iter().map(move |to| (from, to)))
        })
        .collect();
    schedule.initialize(&mut World::new()).unwrap();
    let index: HashMap<_, _> = schedule
        .systems()
        .unwrap()
        .enumerate()
        .map(|(index, (key, _))| (key, index))
        .collect();
    let mut after = vec![Vec::new(); index.len()];
    for (from, to) in edges {
        after[index[&from]].push(index[&to]);
    }
    let mut pairs = HashSet::new();
    for start in 0..index.len() {
        let mut open = after[start].clone();
        while let Some(next) = open.pop() {
            if pairs.insert((start, next)) {
                open.extend(after[next].iter().copied());
            }
        }
    }
    (index.len(), pairs)
}

/// The production configurations of every system that swaps the post-process texture.
fn post_process_schedule() -> Schedule {
    let mut schedule = Schedule::new(Core3d);
    // The inserted command-flush systems swap no texture, so they are left out.
    schedule.set_build_settings(ScheduleBuildSettings {
        auto_insert_apply_deferred: false,
        ..default()
    });
    schedule.add_systems((
        tonemapping,
        crate::retail_hdr_render::post_process_systems(),
        crate::underwater_pass::post_process_systems(),
        SkyrimCinematicPost::schedule_configs(
            fullscreen_material_system::<SkyrimCinematicPost>.into_configs(),
        ),
    ));
    schedule
}

/// How many pairs of systems have no order between them.
fn unordered(schedule: &mut Schedule) -> usize {
    let (count, pairs) = ordered_pairs(schedule);
    (0..count)
        .flat_map(|first| (first + 1..count).map(move |second| (first, second)))
        .filter(|&(first, second)| {
            !pairs.contains(&(first, second)) && !pairs.contains(&(second, first))
        })
        .count()
}

#[test]
fn every_pass_that_swaps_the_post_process_texture_is_ordered() {
    let mut schedule = post_process_schedule();
    assert_eq!(
        unordered(&mut schedule),
        0,
        "two passes that swap the post-process texture have no order between them"
    );
}

/// The check sees the fault that flickered: two passes that only share a predecessor.
#[test]
fn two_passes_after_the_same_system_count_as_unordered() {
    fn first() {}
    fn second() {}
    fn third() {}
    let mut schedule = Schedule::new(Core3d);
    schedule.add_systems((first, second.after(first), third.after(first)));
    assert_eq!(unordered(&mut schedule), 1);
}

/// A new pass that swaps the texture must join the order above, so it is named here.
#[test]
fn no_other_engine_file_swaps_the_post_process_texture() {
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut open = vec![source.clone()];
    let mut found = Vec::new();
    while let Some(directory) = open.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                open.push(path);
                continue;
            }
            if path.extension().is_none_or(|extension| extension != "rs")
                || path.ends_with("post_process_order.rs")
            {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let swaps = text.lines().any(|line| {
                let line = line.trim_start();
                !line.starts_with("//")
                    && (line.contains(".post_process_write()")
                        || line.contains("FullscreenMaterial for "))
            });
            if swaps {
                found.push(path.strip_prefix(&source).unwrap().display().to_string());
            }
        }
    }
    found.sort();
    assert_eq!(
        found, SWAPPING_FILES,
        "give the new pass a place in `post_process_schedule`, then list its file"
    );
}
