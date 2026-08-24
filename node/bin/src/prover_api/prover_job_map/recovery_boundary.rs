use std::collections::VecDeque;

/// SYSCOIN: One immutable startup aggregate. Real recovery ranges contain at least two batches;
/// fake recovery may use a singleton because no expensive wrapper work can be duplicated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PlannedSnarkRange {
    batch_from: u64,
    batch_to: u64,
}

impl PlannedSnarkRange {
    fn new(batch_from: u64, batch_to: u64) -> Self {
        debug_assert!(batch_from <= batch_to);
        Self {
            batch_from,
            batch_to,
        }
    }

    pub(crate) fn batch_from(self) -> u64 {
        self.batch_from
    }

    pub(crate) fn batch_to(self) -> u64 {
        self.batch_to
    }

    pub(crate) fn as_tuple(self) -> (u64, u64) {
        (self.batch_from, self.batch_to)
    }

    pub(crate) fn len(self) -> u64 {
        self.batch_to - self.batch_from + 1
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum StartupRecoveryPlanError {
    #[error("invalid durable-journal range {from}-{to}")]
    InvalidJournalRange { from: u64, to: u64 },
    #[error("real SNARK startup recovery requires aggregate capacity >= 2, got {0}")]
    InsufficientRealCapacity(usize),
    #[error(
        "real SNARK startup recovery would create an interior singleton at batch {batch_number}"
    )]
    InteriorSingleton { batch_number: u64 },
}

/// SYSCOIN: Precomputed startup ownership order. Journal-owned ranges are removed before numeric
/// partitioning, so no live or later aggregate can jump an uncompleted recovery head.
#[derive(Debug)]
pub(crate) struct StartupRecoveryPlan {
    ranges: VecDeque<PlannedSnarkRange>,
    deferred_tip: Option<u64>,
    absolute_tip: Option<u64>,
    fake_mode: bool,
}

impl StartupRecoveryPlan {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build(
        last_proved_batch: u64,
        last_committed_batch: u64,
        validated_journal_ranges: &[(u64, u64)],
        max_fris_per_snark: usize,
        max_assigned_batch_range: usize,
        fake_mode: bool,
    ) -> Result<Self, StartupRecoveryPlanError> {
        for &(from, to) in validated_journal_ranges {
            if from > to {
                return Err(StartupRecoveryPlanError::InvalidJournalRange { from, to });
            }
        }

        let effective_capacity = max_fris_per_snark.min(max_assigned_batch_range.saturating_add(1));
        let Some(first_unproved) = last_proved_batch.checked_add(1) else {
            return Ok(Self::empty(fake_mode));
        };
        if first_unproved > last_committed_batch {
            return Ok(Self::empty(fake_mode));
        }

        let mut covered: Vec<_> = validated_journal_ranges
            .iter()
            .filter_map(|&(from, to)| {
                let from = from.max(first_unproved);
                let to = to.min(last_committed_batch);
                (from <= to).then_some((from, to))
            })
            .collect();
        covered.sort_unstable();
        let mut coalesced = Vec::<(u64, u64)>::new();
        for (from, to) in covered {
            if let Some((_, previous_to)) = coalesced.last_mut()
                && from <= previous_to.saturating_add(1)
            {
                *previous_to = (*previous_to).max(to);
            } else {
                coalesced.push((from, to));
            }
        }

        let mut uncovered = Vec::new();
        let mut cursor = first_unproved;
        let mut exhausted = false;
        for (from, to) in coalesced {
            if cursor < from {
                uncovered.push((cursor, from - 1));
            }
            let Some(next) = to.checked_add(1) else {
                exhausted = true;
                break;
            };
            cursor = cursor.max(next);
            if cursor > last_committed_batch {
                break;
            }
        }
        if !exhausted && cursor <= last_committed_batch {
            uncovered.push((cursor, last_committed_batch));
        }

        let mut ranges = VecDeque::new();
        let mut deferred_tip = None;
        for (from, to) in uncovered {
            Self::partition_segment(
                from,
                to,
                last_committed_batch,
                effective_capacity,
                fake_mode,
                &mut ranges,
                &mut deferred_tip,
            )?;
        }
        Ok(Self {
            ranges,
            deferred_tip,
            absolute_tip: Some(last_committed_batch),
            fake_mode,
        })
    }

    fn empty(fake_mode: bool) -> Self {
        Self {
            ranges: VecDeque::new(),
            deferred_tip: None,
            absolute_tip: None,
            fake_mode,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn partition_segment(
        from: u64,
        to: u64,
        absolute_tip: u64,
        effective_capacity: usize,
        fake_mode: bool,
        ranges: &mut VecDeque<PlannedSnarkRange>,
        deferred_tip: &mut Option<u64>,
    ) -> Result<(), StartupRecoveryPlanError> {
        if effective_capacity == 0 {
            return Err(StartupRecoveryPlanError::InsufficientRealCapacity(0));
        }
        let capacity = effective_capacity as u64;
        let mut cursor = from;
        let mut remaining = to - from + 1;

        while remaining > 0 {
            if fake_mode {
                let take = remaining.min(capacity);
                let range_to = cursor
                    .checked_add(take - 1)
                    .expect("startup recovery chunk is bounded by its segment");
                ranges.push_back(PlannedSnarkRange::new(cursor, range_to));
                cursor = range_to.saturating_add(1);
                remaining -= take;
                continue;
            }

            if remaining == 1 {
                if cursor == absolute_tip {
                    *deferred_tip = Some(cursor);
                    return Ok(());
                }
                return Err(StartupRecoveryPlanError::InteriorSingleton {
                    batch_number: cursor,
                });
            }
            if effective_capacity < 2 {
                return Err(StartupRecoveryPlanError::InsufficientRealCapacity(
                    effective_capacity,
                ));
            }

            let mut take = remaining.min(capacity);
            // SYSCOIN: Rebalance a one-batch remainder into the current aggregate whenever the
            // capacity allows it. In particular, 101 batches at cap 100 become 99 + 2.
            if remaining > capacity && remaining - take == 1 && take > 2 {
                take -= 1;
            }
            let range_to = cursor
                .checked_add(take - 1)
                .expect("startup recovery chunk is bounded by its segment");
            ranges.push_back(PlannedSnarkRange::new(cursor, range_to));
            cursor = range_to.saturating_add(1);
            remaining -= take;
        }
        Ok(())
    }

    pub(crate) fn ranges(&self) -> impl ExactSizeIterator<Item = PlannedSnarkRange> + '_ {
        self.ranges.iter().copied()
    }

    pub(crate) fn deferred_tip(&self) -> Option<u64> {
        self.deferred_tip
    }

    #[cfg(test)]
    pub(crate) fn fake_mode(&self) -> bool {
        self.fake_mode
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StartupRecoveryPhase {
    Live,
    Loading,
    Draining,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum StartupRecoveryBoundaryError {
    #[error("startup SNARK recovery boundary is already installed")]
    AlreadyInstalled,
    #[error("startup SNARK recovery loading has already finished")]
    NotLoading,
    #[error("startup SNARK recovery plan overlaps live queued batch {0}")]
    ActiveJob(u64),
    #[error("startup SNARK recovery plan overlaps completed ownership at batch {0}")]
    AlreadyOwned(u64),
}

/// SYSCOIN: In-process startup sequencing barrier. `Loading` and `Draining` expose only the exact
/// first planned range; an empty non-Live boundary still blocks later/live work until loading ends.
#[derive(Debug)]
pub(super) struct SnarkRecoveryBoundary {
    phase: StartupRecoveryPhase,
    installed: bool,
    pending: VecDeque<PlannedSnarkRange>,
    fake_mode: bool,
    deferred_tip: Option<u64>,
    absolute_tip: Option<u64>,
}

impl Default for SnarkRecoveryBoundary {
    fn default() -> Self {
        Self {
            phase: StartupRecoveryPhase::Live,
            installed: false,
            pending: VecDeque::new(),
            fake_mode: false,
            deferred_tip: None,
            absolute_tip: None,
        }
    }
}

impl SnarkRecoveryBoundary {
    pub(super) fn install(
        &mut self,
        plan: StartupRecoveryPlan,
    ) -> Result<(), StartupRecoveryBoundaryError> {
        if self.installed {
            return Err(StartupRecoveryBoundaryError::AlreadyInstalled);
        }
        self.installed = true;
        self.phase = StartupRecoveryPhase::Loading;
        self.pending = plan.ranges;
        self.fake_mode = plan.fake_mode;
        self.deferred_tip = plan.deferred_tip;
        self.absolute_tip = plan.absolute_tip;
        if let Some(deferred_tip) = self.deferred_tip {
            self.pending
                .push_back(PlannedSnarkRange::new(deferred_tip, deferred_tip));
        }
        Ok(())
    }

    pub(super) fn finish_loading(&mut self) -> Result<(), StartupRecoveryBoundaryError> {
        if self.phase != StartupRecoveryPhase::Loading {
            return Err(StartupRecoveryBoundaryError::NotLoading);
        }
        self.phase = if self.pending.is_empty() {
            StartupRecoveryPhase::Live
        } else {
            StartupRecoveryPhase::Draining
        };
        Ok(())
    }

    pub(super) fn phase(&self) -> StartupRecoveryPhase {
        self.phase
    }

    pub(super) fn head(&self) -> Option<PlannedSnarkRange> {
        self.pending.front().copied()
    }

    #[cfg(test)]
    pub(super) fn deferred_tip(&self) -> Option<u64> {
        self.deferred_tip
    }

    /// SYSCOIN: The first contiguous post-startup FRI turns a deferred real tip singleton into a
    /// normal two-proof head. This happens atomically with live-map insertion under boundary→jobs.
    pub(super) fn observe_admission(&mut self, batch_number: u64) {
        let Some(deferred_tip) = self.deferred_tip else {
            return;
        };
        if deferred_tip.checked_add(1) != Some(batch_number) {
            return;
        }
        let Some(last) = self.pending.back_mut() else {
            return;
        };
        if last.as_tuple() == (deferred_tip, deferred_tip) {
            *last = PlannedSnarkRange::new(deferred_tip, batch_number);
            self.deferred_tip = None;
        }
    }

    pub(super) fn can_defer_tip_after(&self, completed_to: u64) -> bool {
        self.head().is_some_and(|head| {
            completed_to.checked_add(1) == self.absolute_tip
                && head.batch_to() == self.absolute_tip.unwrap_or_default()
        })
    }

    pub(super) fn can_complete_head(&self, completed: (u64, u64)) -> bool {
        if self.phase == StartupRecoveryPhase::Live {
            return true;
        }
        self.head().is_some_and(|head| {
            completed.0 == head.batch_from()
                && completed.1 >= completed.0
                && completed.1 <= head.batch_to()
        })
    }

    pub(super) fn complete_head(&mut self, completed: (u64, u64)) -> bool {
        if !self.can_complete_head(completed) {
            return false;
        }
        if self.phase == StartupRecoveryPhase::Live {
            return true;
        }
        let head = self
            .head()
            .expect("validated startup recovery head disappeared");
        if completed.1 == head.batch_to() {
            self.pending.pop_front();
            if self.deferred_tip == Some(head.batch_from()) && head.len() == 1 {
                self.deferred_tip = None;
            }
        } else {
            let remainder_from = completed.1 + 1;
            if remainder_from == head.batch_to()
                && Some(remainder_from) == self.absolute_tip
                && !self.fake_mode
            {
                *self
                    .pending
                    .front_mut()
                    .expect("startup recovery head disappeared") =
                    PlannedSnarkRange::new(remainder_from, remainder_from);
                self.deferred_tip = Some(remainder_from);
            } else {
                *self
                    .pending
                    .front_mut()
                    .expect("startup recovery head disappeared") =
                    PlannedSnarkRange::new(remainder_from, head.batch_to());
            }
        }
        if self.phase == StartupRecoveryPhase::Draining && self.pending.is_empty() {
            self.phase = StartupRecoveryPhase::Live;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tuples(plan: &StartupRecoveryPlan) -> Vec<(u64, u64)> {
        plan.ranges().map(PlannedSnarkRange::as_tuple).collect()
    }

    #[test]
    fn real_101_at_capacity_100_rebalances_to_99_plus_2() {
        let plan = StartupRecoveryPlan::build(0, 101, &[], 100, 255, false).unwrap();
        assert_eq!(tuples(&plan), vec![(1, 99), (100, 101)]);
        assert_eq!(plan.deferred_tip(), None);
    }

    #[test]
    fn effective_capacity_is_minimum_of_wrapper_and_map_bounds() {
        let plan = StartupRecoveryPlan::build(0, 10, &[], 100, 3, false).unwrap();
        assert_eq!(tuples(&plan), vec![(1, 4), (5, 8), (9, 10)]);
    }

    #[test]
    fn real_plan_defers_only_absolute_tip_singleton() {
        let plan = StartupRecoveryPlan::build(4, 5, &[], 100, 255, false).unwrap();
        assert!(tuples(&plan).is_empty());
        assert_eq!(plan.deferred_tip(), Some(5));

        assert_eq!(
            StartupRecoveryPlan::build(0, 4, &[(2, 4)], 100, 255, false).unwrap_err(),
            StartupRecoveryPlanError::InteriorSingleton { batch_number: 1 }
        );
    }

    #[test]
    fn fake_plan_may_use_singletons() {
        let plan = StartupRecoveryPlan::build(4, 5, &[], 100, 255, true).unwrap();
        assert_eq!(tuples(&plan), vec![(5, 5)]);
        assert_eq!(plan.deferred_tip(), None);
        assert!(plan.fake_mode());
    }

    #[test]
    fn journal_ranges_are_clipped_coalesced_and_excluded() {
        let plan = StartupRecoveryPlan::build(
            10,
            20,
            &[(1, 11), (12, 13), (12, 14), (21, 30)],
            3,
            255,
            false,
        )
        .unwrap();
        assert_eq!(tuples(&plan), vec![(15, 17), (18, 20)]);
    }

    #[test]
    fn coverage_through_u64_max_does_not_recreate_the_tip() {
        let plan = StartupRecoveryPlan::build(
            u64::MAX - 2,
            u64::MAX,
            &[(u64::MAX - 1, u64::MAX)],
            100,
            255,
            false,
        )
        .unwrap();
        assert!(tuples(&plan).is_empty());
        assert_eq!(plan.deferred_tip(), None);

        let uncovered_tip =
            StartupRecoveryPlan::build(u64::MAX - 2, u64::MAX, &[], 2, 255, false).unwrap();
        assert_eq!(tuples(&uncovered_tip), vec![(u64::MAX - 1, u64::MAX)]);
    }

    #[test]
    fn deferred_tip_blocks_draining_until_a_successor_extends_it() {
        let plan = StartupRecoveryPlan::build(4, 5, &[], 100, 255, false).unwrap();
        let mut boundary = SnarkRecoveryBoundary::default();
        boundary.install(plan).unwrap();
        assert_eq!(boundary.phase(), StartupRecoveryPhase::Loading);
        assert_eq!(boundary.head().unwrap().as_tuple(), (5, 5));

        boundary.finish_loading().unwrap();
        assert_eq!(boundary.phase(), StartupRecoveryPhase::Draining);
        assert!(!boundary.complete_head((5, 6)));

        boundary.observe_admission(6);
        assert_eq!(boundary.head().unwrap().as_tuple(), (5, 6));
        assert_eq!(boundary.deferred_tip(), None);
        assert!(boundary.complete_head((5, 6)));
        assert_eq!(boundary.phase(), StartupRecoveryPhase::Live);
        assert!(boundary.head().is_none());
    }

    #[test]
    fn absolute_tip_prefix_completion_defers_remainder_and_install_is_one_shot() {
        let plan = StartupRecoveryPlan::build(0, 3, &[], 3, 255, false).unwrap();
        let mut boundary = SnarkRecoveryBoundary::default();
        boundary.install(plan).unwrap();
        boundary.finish_loading().unwrap();

        assert!(boundary.complete_head((1, 2)));
        assert_eq!(boundary.phase(), StartupRecoveryPhase::Draining);
        assert_eq!(boundary.head().unwrap().as_tuple(), (3, 3));
        assert_eq!(boundary.deferred_tip(), Some(3));

        let empty = StartupRecoveryPlan::build(3, 3, &[], 3, 255, false).unwrap();
        assert_eq!(
            boundary.install(empty),
            Err(StartupRecoveryBoundaryError::AlreadyInstalled)
        );
    }
}
