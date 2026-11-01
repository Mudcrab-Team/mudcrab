//! Immutable source-order metadata, rebuilt only when a view's draw groups change.
//!
//! No visibility is decided here. The GPU prefixes current selected counts in this order.
//! Words: header (groups, tiles, ordered-instance-count, large-list-base), four words per group
//! (first tile, tile count, argument base, reserved), four per tile (first instance word,
//! instance count, group, reserved), then source instance indices, then a count and
//! indices of instances with more than 64 clusters (cooperative scatter work).

pub(crate) const TILE_SIZE: usize = 256;

pub(crate) fn source_order(
    mut instances: Vec<Vec<u32>>,
    bases: &[u32],
    large_instances: &[u32],
    ordered_instances: u32,
) -> Vec<u32> {
    assert_eq!(instances.len(), bases.len());
    let tile_count: usize = instances
        .iter()
        .map(|ids| ids.len().div_ceil(TILE_SIZE))
        .sum();
    let instance_base = 4 + instances.len() * 4 + tile_count * 4;
    let mut words = vec![
        instances.len() as u32,
        tile_count as u32,
        ordered_instances,
        0,
    ];
    let mut first_tile = 0;
    for (ids, &base) in instances.iter().zip(bases) {
        let tiles = ids.len().div_ceil(TILE_SIZE) as u32;
        words.extend([first_tile, tiles, base, 0]);
        first_tile += tiles;
    }
    let mut next_instance = instance_base as u32;
    for (group, ids) in instances.iter_mut().enumerate() {
        ids.sort_unstable();
        for tile in ids.chunks(TILE_SIZE) {
            words.extend([next_instance, tile.len() as u32, group as u32, 0]);
            next_instance += tile.len() as u32;
        }
    }
    for ids in instances {
        words.extend(ids);
    }
    words[3] = words.len() as u32;
    words.push(large_instances.len() as u32);
    words.extend_from_slice(large_instances);
    words
}

/// One source order, shared by every view whose draw groups have the same membership under the
/// same instance layout. The order is view independent: it follows from which batches each group
/// holds and from the instances that have arrived (`epoch`), never from what a view sees.
pub(crate) struct SharedOrder {
    epoch: u64,
    /// Per group: its representative batch and its slots.
    groups: Vec<(u32, u32)>,
    /// Per batch: its group, if any.
    batch_group: Vec<Option<u32>>,
    pub(crate) words: Vec<u32>,
}

/// The source orders the views currently hold. An order is built once for its inputs and handed
/// to every view (camera or shadow) that asks with the same inputs; an entry no view holds any
/// more is dropped.
#[derive(Default)]
pub(crate) struct OrderCache {
    entries: Vec<std::sync::Weak<SharedOrder>>,
    /// How many orders were built, for the queue report and the tests.
    #[cfg(test)]
    pub(crate) builds: u64,
}

impl OrderCache {
    /// The order for these groups under `epoch`, built by `build` only when no view holds it.
    pub(crate) fn get_or_build(
        &mut self,
        epoch: u64,
        groups: &[(u32, u32)],
        batch_group: &[Option<u32>],
        build: impl FnOnce() -> Vec<u32>,
    ) -> std::sync::Arc<SharedOrder> {
        self.entries.retain(|entry| entry.strong_count() > 0);
        if let Some(found) = self
            .entries
            .iter()
            .filter_map(std::sync::Weak::upgrade)
            .find(|entry| {
                entry.epoch == epoch && entry.groups == groups && entry.batch_group == batch_group
            })
        {
            return found;
        }
        #[cfg(test)]
        {
            self.builds += 1;
        }
        let entry = std::sync::Arc::new(SharedOrder {
            epoch,
            groups: groups.to_vec(),
            batch_group: batch_group.to_vec(),
            words: build(),
        });
        self.entries.push(std::sync::Arc::downgrade(&entry));
        entry
    }
}

/// Includes the partial tile of every group, even if each group has just one instance.
pub(crate) fn order_capacity(instances: u32, groups: usize) -> u64 {
    let tiles = instances.div_ceil(TILE_SIZE as u32) as u64 + groups as u64;
    (5 + groups as u64 * 4 + tiles * 4 + u64::from(instances) * 2) * 4
}

pub(crate) fn prefix_capacity(instances: u32, groups: usize) -> u64 {
    u64::from(instances.div_ceil(64))
        .max(u64::from(instances.div_ceil(TILE_SIZE as u32)) + groups as u64)
        .max(1)
        .saturating_add(1)
        * 8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiles_preserve_source_order_across_groups_and_boundaries() {
        let ids: Vec<_> = (0..513).rev().collect();
        let words = source_order(
            vec![ids, vec![], vec![900, 700]],
            &[0, 600, 600],
            &[900],
            901,
        );
        assert_eq!(
            &words[..16],
            &[3, 4, 901, 547, 0, 3, 0, 0, 3, 0, 600, 0, 3, 1, 600, 0]
        );
        assert_eq!(
            &words[16..32],
            &[32, 256, 0, 0, 288, 256, 0, 0, 544, 1, 0, 0, 545, 2, 2, 0]
        );
        assert_eq!(&words[32..545], &(0..513).collect::<Vec<_>>());
        assert_eq!(&words[545..], &[700, 900, 1, 900]);
        assert!(words.len() as u64 * 4 <= order_capacity(515, 3));
    }

    /// Unchanged inputs build nothing, whatever the number of views or frames; a changed input
    /// builds exactly once and gives what a build from scratch gives.
    #[test]
    fn shared_orders_are_built_once_per_input() {
        let build = |groups: &[Vec<u32>]| {
            let bases: Vec<u32> = (0..groups.len() as u32).map(|group| group * 10).collect();
            source_order(groups.to_vec(), &bases, &[], 20)
        };
        let mut cache = OrderCache::default();
        let groups = [(0, 4), (2, 6)];
        let membership = [Some(0), None, Some(1)];
        let ids = vec![vec![3, 1], vec![2, 0]];
        let first = cache.get_or_build(1, &groups, &membership, || build(&ids));
        // A thousand frames over four views: no rebuild.
        for _ in 0..1000 {
            for _ in 0..4 {
                let again = cache.get_or_build(1, &groups, &membership, || unreachable!());
                assert!(std::sync::Arc::ptr_eq(&first, &again));
            }
        }
        assert_eq!(cache.builds, 1);
        // Another membership: one build, equal to a build from scratch, the old one still served.
        let other = [Some(0), Some(0), Some(1)];
        let changed = cache.get_or_build(1, &groups, &other, || build(&[vec![3], vec![2]]));
        assert_eq!(cache.builds, 2);
        assert_eq!(changed.words, build(&[vec![3], vec![2]]));
        assert!(std::sync::Arc::ptr_eq(
            &first,
            &cache.get_or_build(1, &groups, &membership, || unreachable!())
        ));
        // New instances (another epoch) rebuild; orders no view holds are dropped.
        drop((first, changed));
        let next = cache.get_or_build(2, &groups, &membership, || build(&ids));
        assert_eq!(cache.builds, 3);
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(next.words, build(&ids));
    }

    #[test]
    fn empty_and_singleton_groups_fit_the_capacity_bound() {
        for n in [0, 1, 63, 64, 255, 256, 257, 65_536] {
            let words = source_order(
                (0..n).map(|id| vec![id]).collect(),
                &vec![0; n as usize],
                &(0..n).collect::<Vec<_>>(),
                n,
            );
            assert!(words.len() as u64 * 4 <= order_capacity(n, n as usize));
            assert!(prefix_capacity(n, n as usize) >= n as u64 * 8);
        }
    }
}
