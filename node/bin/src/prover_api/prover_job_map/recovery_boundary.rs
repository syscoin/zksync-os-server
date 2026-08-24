use std::collections::VecDeque;

// SYSCOIN: Bound startup ownership planning independently of the committed backlog. The live job
// map remains much smaller, but a corrupt or unexpectedly distant frontier must not materialize an
// unbounded number of future wrapper ranges before recovery can begin draining it.
pub(crate) const MAX_STARTUP_RECOVERY_RANGES: usize = 65_536;

/// SYSCOIN: One startup aggregate boundary. Real recovery ranges contain at least two batches;
/// fake recovery may use a singleton because no expensive wrapper work can be duplicated. A real
/// boundary may be repartitioned before leasing when a runtime byte cap is tighter than its
/// count-planned size, but its ordered numeric coverage remains unchanged.
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
    #[error(
        "SNARK startup recovery requires at least {required_at_least} ranges; maximum is {max}"
    )]
    TooManyRanges {
        required_at_least: usize,
        max: usize,
    },
    #[error("SNARK startup recovery received {provided} journal ranges; maximum is {max}")]
    TooManyJournalRanges { provided: usize, max: usize },
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
        // SYSCOIN: Journal filenames are durable input discovered before the bounded live map exists.
        // Reject an excessive set before cloning it into covered/coalesced/uncovered planning vectors.
        if validated_journal_ranges.len() > MAX_STARTUP_RECOVERY_RANGES {
            return Err(StartupRecoveryPlanError::TooManyJournalRanges {
                provided: validated_journal_ranges.len(),
                max: MAX_STARTUP_RECOVERY_RANGES,
            });
        }
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
                Self::push_range(ranges, PlannedSnarkRange::new(cursor, range_to))?;
                cursor = range_to.saturating_add(1);
                remaining -= take;
                continue;
            }

            if remaining == 1 {
                if cursor == absolute_tip {
                    // SYSCOIN: The deferred singleton is installed into the same pending deque once
                    // its contiguous successor arrives, so reserve its range budget at plan time.
                    Self::ensure_range_budget(ranges.len(), 1)?;
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
            Self::push_range(ranges, PlannedSnarkRange::new(cursor, range_to))?;
            cursor = range_to.saturating_add(1);
            remaining -= take;
        }
        Ok(())
    }

    fn push_range(
        ranges: &mut VecDeque<PlannedSnarkRange>,
        range: PlannedSnarkRange,
    ) -> Result<(), StartupRecoveryPlanError> {
        Self::ensure_range_budget(ranges.len(), 1)?;
        ranges.push_back(range);
        Ok(())
    }

    fn ensure_range_budget(
        current: usize,
        additional: usize,
    ) -> Result<(), StartupRecoveryPlanError> {
        let required_at_least =
            current
                .checked_add(additional)
                .ok_or(StartupRecoveryPlanError::TooManyRanges {
                    required_at_least: usize::MAX,
                    max: MAX_STARTUP_RECOVERY_RANGES,
                })?;
        if required_at_least > MAX_STARTUP_RECOVERY_RANGES {
            return Err(StartupRecoveryPlanError::TooManyRanges {
                required_at_least,
                max: MAX_STARTUP_RECOVERY_RANGES,
            });
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

    /// SYSCOIN: Expose only the immediately adjacent pending range. A journal-owned gap is an
    /// immutable ownership boundary and can never be crossed to rescue a real singleton.
    pub(super) fn next_contiguous_range(&self) -> Option<PlannedSnarkRange> {
        let mut pending = self.pending.iter().copied();
        let head = pending.next()?;
        let next = pending.next()?;
        (head.batch_to().checked_add(1) == Some(next.batch_from())).then_some(next)
    }

    /// SYSCOIN: A runtime response/journal byte cap can repeatedly lease a shorter prefix than the
    /// count-based startup plan anticipated. If that prefix would leave one interior batch, move
    /// the boundary across the next contiguous compatible range before any lease exists. This
    /// preserves every batch exactly once while keeping all real ranges within `max_range_len`.
    pub(super) fn repartition_head_after_prefix(
        &mut self,
        completed_to: u64,
        max_range_len: usize,
    ) -> bool {
        if self.fake_mode || max_range_len < 2 {
            return false;
        }
        let Some(head) = self.head() else {
            return false;
        };
        if completed_to < head.batch_from() || completed_to.checked_add(1) != Some(head.batch_to())
        {
            return false;
        }
        let Some(next) = self.next_contiguous_range() else {
            return false;
        };
        let prefix_len = completed_to - head.batch_from() + 1;
        let max_range_len = max_range_len as u64;
        let next_len = next.len();
        let next_after_first = next.batch_from().checked_add(1);

        // Move only the successor's first batch across its old boundary. This is important for
        // compatibility: the picker needs to authenticate one newly adjacent FRI, never a future
        // range that has not entered the bounded resident map yet.
        let replacement = if next_len < max_range_len {
            vec![
                PlannedSnarkRange::new(head.batch_from(), completed_to),
                PlannedSnarkRange::new(head.batch_to(), next.batch_to()),
            ]
        } else if prefix_len.saturating_add(2) <= max_range_len && next_len >= 3 {
            vec![
                PlannedSnarkRange::new(head.batch_from(), next.batch_from()),
                PlannedSnarkRange::new(
                    next_after_first.expect("multi-batch successor cannot end at u64::MAX"),
                    next.batch_to(),
                ),
            ]
        } else {
            let Some(next_remainder_from) = next_after_first else {
                return false;
            };
            // SYSCOIN: Repartitioning a full 3+3 boundary into 2+2+2 necessarily needs one
            // transient extra deque entry until the leased prefix completes. Startup planning is
            // still capped at MAX_STARTUP_RECOVERY_RANGES, and no second repartition can pass this
            // head before that completion, so the live bound is exactly MAX+1 rather than a fatal
            // cap-edge singleton.
            if next_len < 3 || self.pending.len() > MAX_STARTUP_RECOVERY_RANGES {
                return false;
            }
            vec![
                PlannedSnarkRange::new(head.batch_from(), completed_to),
                PlannedSnarkRange::new(head.batch_to(), next.batch_from()),
                PlannedSnarkRange::new(next_remainder_from, next.batch_to()),
            ]
        };

        debug_assert!(replacement.iter().all(|range| {
            let len = range.len();
            (2..=max_range_len).contains(&len)
        }));
        let replaced_deferred_tip = self.deferred_tip == Some(next.batch_from()) && next.len() == 1;
        let mut untouched = self.pending.split_off(2);
        self.pending.clear();
        self.pending.extend(replacement);
        self.pending.append(&mut untouched);
        debug_assert!(self.pending.len() <= MAX_STARTUP_RECOVERY_RANGES + 1);
        if replaced_deferred_tip {
            self.deferred_tip = None;
        }

        debug_assert!(self.can_complete_head((head.batch_from(), completed_to)));
        true
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
    fn runtime_prefix_repartitions_full_successor_without_exceeding_wrapper_max() {
        let mut boundary = SnarkRecoveryBoundary::default();
        boundary
            .install(StartupRecoveryPlan::build(0, 10, &[], 5, 100, false).unwrap())
            .unwrap();
        boundary.finish_loading().unwrap();

        assert!(boundary.complete_head((1, 2)));
        assert_eq!(boundary.head().unwrap().as_tuple(), (3, 5));
        assert!(boundary.repartition_head_after_prefix(4, 5));
        assert_eq!(
            boundary
                .pending
                .iter()
                .copied()
                .map(PlannedSnarkRange::as_tuple)
                .collect::<Vec<_>>(),
            vec![(3, 6), (7, 10)]
        );

        // The picker leases only 3-4. Completing that prefix leaves the repaired 5-6 pair.
        assert!(boundary.complete_head((3, 4)));
        assert_eq!(boundary.head().unwrap().as_tuple(), (5, 6));
    }

    #[test]
    fn runtime_prefix_splits_full_three_fri_successor_into_real_pairs() {
        let mut boundary = SnarkRecoveryBoundary::default();
        boundary
            .install(StartupRecoveryPlan::build(0, 6, &[], 3, 100, false).unwrap())
            .unwrap();
        boundary.finish_loading().unwrap();

        assert!(boundary.repartition_head_after_prefix(2, 3));
        assert_eq!(
            boundary
                .pending
                .iter()
                .copied()
                .map(PlannedSnarkRange::as_tuple)
                .collect::<Vec<_>>(),
            vec![(1, 2), (3, 4), (5, 6)]
        );
        assert!(
            boundary
                .pending
                .iter()
                .all(|range| (2..=3).contains(&range.len()))
        );
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
    fn startup_plan_accepts_exact_range_cap() {
        let last_committed = MAX_STARTUP_RECOVERY_RANGES as u64;
        let plan = StartupRecoveryPlan::build(0, last_committed, &[], 1, 0, true).unwrap();

        assert_eq!(plan.ranges().len(), MAX_STARTUP_RECOVERY_RANGES);
        assert_eq!(plan.deferred_tip(), None);
    }

    #[test]
    fn startup_plan_rejects_range_count_above_cap() {
        let required_at_least = MAX_STARTUP_RECOVERY_RANGES + 1;
        let error =
            StartupRecoveryPlan::build(0, required_at_least as u64, &[], 1, 0, true).unwrap_err();

        assert_eq!(
            error,
            StartupRecoveryPlanError::TooManyRanges {
                required_at_least,
                max: MAX_STARTUP_RECOVERY_RANGES,
            }
        );
    }

    #[test]
    fn startup_plan_rejects_journal_input_above_cap_before_collecting() {
        let provided = MAX_STARTUP_RECOVERY_RANGES + 1;
        let journal_ranges = vec![(1, 2); provided];
        let error = StartupRecoveryPlan::build(0, 2, &journal_ranges, 2, 1, false).unwrap_err();

        assert_eq!(
            error,
            StartupRecoveryPlanError::TooManyJournalRanges {
                provided,
                max: MAX_STARTUP_RECOVERY_RANGES,
            }
        );
    }

    #[test]
    fn deferred_real_tip_counts_toward_range_cap() {
        let required_at_least = MAX_STARTUP_RECOVERY_RANGES + 1;
        let last_committed = (MAX_STARTUP_RECOVERY_RANGES as u64) * 2 + 1;
        let error = StartupRecoveryPlan::build(0, last_committed, &[], 2, 1, false).unwrap_err();

        assert_eq!(
            error,
            StartupRecoveryPlanError::TooManyRanges {
                required_at_least,
                max: MAX_STARTUP_RECOVERY_RANGES,
            }
        );
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
