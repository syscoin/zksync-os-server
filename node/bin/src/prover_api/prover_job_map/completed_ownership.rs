use std::collections::BTreeMap;

/// SYSCOIN: Permanent in-process tombstones for FRI batches already consumed by a SNARK wrapper.
/// Sequential completion remains one interval instead of growing one entry per batch or wrapper.
#[derive(Debug, Default)]
pub(super) struct SnarkCompletedOwnership {
    ranges: BTreeMap<u64, u64>,
}

impl SnarkCompletedOwnership {
    pub(super) fn contains(&self, batch_number: u64) -> bool {
        self.ranges
            .range(..=batch_number)
            .next_back()
            .is_some_and(|(_, &to)| to >= batch_number)
    }

    pub(super) fn overlaps(&self, from: u64, to: u64) -> bool {
        self.first_overlap(from, to).is_some()
    }

    pub(super) fn first_overlap(&self, from: u64, to: u64) -> Option<u64> {
        self.ranges
            .range(..=to)
            .next_back()
            .and_then(|(&owned_from, &owned_to)| (owned_to >= from).then_some(owned_from.max(from)))
    }

    pub(super) fn claim(&mut self, from: u64, to: u64) {
        debug_assert!(
            from <= to,
            "completed SNARK ownership range must not be inverted"
        );

        let mut merged_from = from;
        let mut merged_to = to;
        if let Some((&previous_from, &previous_to)) = self.ranges.range(..=from).next_back()
            && previous_to.saturating_add(1) >= from
        {
            merged_from = previous_from;
            merged_to = merged_to.max(previous_to);
            self.ranges.remove(&previous_from);
        }

        while let Some((&next_from, &next_to)) = self.ranges.range(merged_from..).next() {
            if next_from > merged_to.saturating_add(1) {
                break;
            }
            merged_to = merged_to.max(next_to);
            self.ranges.remove(&next_from);
        }

        self.ranges.insert(merged_from, merged_to);
    }

    #[cfg(test)]
    pub(super) fn ranges(&self) -> Vec<(u64, u64)> {
        self.ranges.iter().map(|(&from, &to)| (from, to)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ownership_ranges_coalesce_across_adjacent_ranges_and_u64_boundary() {
        let mut ownership = SnarkCompletedOwnership::default();
        ownership.claim(5, 7);
        ownership.claim(9, 10);
        ownership.claim(8, 8);
        ownership.claim(u64::MAX, u64::MAX);

        assert_eq!(ownership.ranges(), vec![(5, 10), (u64::MAX, u64::MAX)]);
        assert!(ownership.contains(6));
        assert!(ownership.overlaps(7, 9));
        assert!(ownership.contains(u64::MAX));
        assert!(!ownership.contains(11));
    }
}
