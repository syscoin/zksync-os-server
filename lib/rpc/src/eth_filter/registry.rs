use super::EthFilterError;
use super::pending::PendingTransactionKind;
use alloy::primitives::U128;
use alloy::rpc::types::{Filter, FilterId};
use dashmap::DashMap;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};
use tokio::time::MissedTickBehavior;

/// An active installed filter
#[derive(Debug)]
pub(crate) struct ActiveFilter {
    /// The first block not yet returned by a successful change poll.
    pub(crate) block: u64,
    /// Last time this filter was polled.
    pub(crate) last_poll_timestamp: Instant,
    /// What kind of filter it is.
    pub(crate) kind: FilterKind,
    /// SYSCOIN: Release retained capacity when the entry (including its listener) is dropped.
    _reservation: FilterReservation,
}

// SYSCOIN: Entry-owned reservations cover uninstall, expiry, replacement, and failed construction.
#[derive(Debug)]
struct FilterReservation {
    retained: Arc<AtomicUsize>,
}

impl Drop for FilterReservation {
    fn drop(&mut self) {
        self.retained.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone, Debug)]
pub(crate) enum FilterKind {
    Log(Box<Filter>),
    Block,
    PendingTransaction(PendingTransactionKind),
}

impl FilterKind {
    pub(crate) fn as_log_filter(&self) -> Option<&Filter> {
        if let Self::Log(filter) = self {
            Some(filter)
        } else {
            None
        }
    }
}

/// Manages installed filters, serialized change polls, and stale-filter eviction.
#[derive(Clone)]
pub(crate) struct FilterRegistry {
    filters: Arc<DashMap<FilterId, Arc<Mutex<ActiveFilter>>>>,
    stale_filter_ttl: Duration,
    // SYSCOIN: Count reservations atomically across clones; map length checks race with installs.
    retained: Arc<AtomicUsize>,
    // SYSCOIN: Bound both retained entries and each log filter's variable-sized criteria.
    max_active_filters: NonZeroU32,
    max_filter_terms: NonZeroU32,
}

impl FilterRegistry {
    pub(crate) fn new(
        stale_filter_ttl: Duration,
        max_active_filters: NonZeroU32,
        max_filter_terms: NonZeroU32,
    ) -> Self {
        Self {
            filters: Arc::new(DashMap::new()),
            stale_filter_ttl,
            retained: Arc::new(AtomicUsize::new(0)),
            max_active_filters,
            max_filter_terms,
        }
    }

    /// SYSCOIN: Bound retained variable-sized criteria, not just the installed-filter count.
    /// Rebuild the sets so excess input allocation capacity and cached blooms are not retained.
    /// Stateless `eth_getLogs` does not pass through this installation-only limit.
    pub(crate) fn bounded_log_filter(&self, mut filter: Filter) -> Result<Filter, EthFilterError> {
        let terms = filter
            .topics
            .iter()
            .fold(filter.address.len(), |total, topic| {
                total.saturating_add(topic.len())
            });
        if terms > self.max_filter_terms.get() as usize {
            return Err(EthFilterError::FilterCriteriaTooLarge {
                max_terms: self.max_filter_terms.get(),
            });
        }
        filter.address = filter.address.iter().copied().collect();
        for topic in &mut filter.topics {
            *topic = topic.iter().copied().collect();
        }
        Ok(filter)
    }

    // SYSCOIN: Reserve atomically before allocation rather than checking a concurrent map's len.
    fn reserve(&self) -> Result<FilterReservation, EthFilterError> {
        self.retained
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |retained| {
                (retained < self.max_active_filters.get() as usize).then_some(retained + 1)
            })
            .map_err(|_| EthFilterError::FilterCapacityReached {
                max_filters: self.max_active_filters.get(),
            })?;
        Ok(FilterReservation {
            retained: self.retained.clone(),
        })
    }

    /// Installs a new filter with the first block to include in change polling.
    /// SYSCOIN: Reserve shared capacity before constructing the kind, particularly listeners.
    pub(crate) fn install_with(
        &self,
        make_kind: impl FnOnce() -> FilterKind,
        start_block: u64,
    ) -> Result<FilterId, EthFilterError> {
        let reservation = self.reserve()?;
        let id = FilterId::Str(format!("0x{:x}", U128::random()));
        self.filters.insert(
            id.clone(),
            Arc::new(Mutex::new(ActiveFilter {
                block: start_block,
                last_poll_timestamp: Instant::now(),
                kind: make_kind(),
                _reservation: reservation,
            })),
        );
        Ok(id)
    }

    /// Removes an installed filter. Returns `true` if it existed.
    pub(crate) fn uninstall(&self, id: &FilterId) -> bool {
        self.filters.remove(id).is_some()
    }

    /// Returns the log `Filter` for a filter ID, or an error if it does not exist or is not a log
    /// filter.
    pub(crate) fn get_log_filter(&self, id: &FilterId) -> Result<Filter, EthFilterError> {
        let entry = self
            .filters
            .get(id)
            .map(|entry| Arc::clone(entry.value()))
            .ok_or_else(|| EthFilterError::FilterNotFound(id.clone()))?;
        let entry = entry.lock().expect("filter lock poisoned");
        entry
            .kind
            .as_log_filter()
            .cloned()
            .ok_or_else(|| EthFilterError::FilterNotFound(id.clone()))
    }

    // SYSCOIN: Serialize a filter's polls and commit its cursor only after a successful scan.
    // Use an entry lock so disk scans never hold a shared registry shard lock.
    // Idle polls still renew the lease, and pending notifications do not depend on a new block.
    pub(crate) fn poll<T>(
        &self,
        id: FilterId,
        read_changes: impl FnOnce(u64, &FilterKind) -> Result<(T, u64), EthFilterError>,
    ) -> Result<T, EthFilterError> {
        let entry = self
            .filters
            .get(&id)
            .map(|entry| Arc::clone(entry.value()))
            .ok_or(EthFilterError::FilterNotFound(id))?;
        let mut entry = entry.lock().expect("filter lock poisoned");

        entry.last_poll_timestamp = Instant::now();
        let (changes, next_block) = read_changes(entry.block, &entry.kind)?;
        entry.block = next_block;
        Ok(changes)
    }

    /// Evicts filters that have not been polled within `stale_filter_ttl`.
    pub(crate) fn clear_stale(&self, now: Instant) {
        self.filters.retain(|id, filter| {
            // SYSCOIN: A currently executing poll is active; never wait for its disk scan here.
            let filter = match filter.try_lock() {
                Ok(filter) => filter,
                Err(TryLockError::WouldBlock) => return true,
                Err(TryLockError::Poisoned(_)) => panic!("filter lock poisoned"),
            };
            let is_valid = (now - filter.last_poll_timestamp) < self.stale_filter_ttl;
            if !is_valid {
                tracing::trace!(?id, "evicting stale filter");
            }
            is_valid
        });
    }

    /// Runs an endless loop that calls [`Self::clear_stale`] on every `stale_filter_ttl` tick.
    pub(crate) async fn watch_and_clear_stale(&self) {
        let mut interval = tokio::time::interval_at(
            tokio::time::Instant::now() + self.stale_filter_ttl,
            self.stale_filter_ttl,
        );
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            self.clear_stale(Instant::now());
        }
    }
}

#[cfg(test)]
// SYSCOIN: Freeze retained-capacity, criteria-size, and listener-lifecycle invariants.
mod tests {
    use super::*;
    use crate::eth_filter::pending::PendingTransactionsReceiver;
    use alloy::primitives::{Address, B256};
    use std::sync::Barrier;

    fn registry(max_filters: u32, max_terms: u32) -> FilterRegistry {
        FilterRegistry::new(
            Duration::from_secs(60),
            NonZeroU32::new(max_filters).unwrap(),
            NonZeroU32::new(max_terms).unwrap(),
        )
    }

    #[test]
    fn capacity_is_shared_and_reclaimed_on_uninstall_and_expiry() {
        let registry = registry(1, 4);
        let clone = registry.clone();
        let id = registry.install_with(|| FilterKind::Block, 7).unwrap();
        let mut made_kind = false;
        assert!(matches!(
            clone.install_with(
                || {
                    made_kind = true;
                    FilterKind::Block
                },
                7,
            ),
            Err(EthFilterError::FilterCapacityReached { max_filters: 1 })
        ));
        assert!(
            !made_kind,
            "capacity rejection must precede listener construction"
        );
        assert!(clone.uninstall(&id));
        assert!(!clone.uninstall(&id));
        let id = clone.install_with(|| FilterKind::Block, 7).unwrap();
        registry.clear_stale(Instant::now() + Duration::from_secs(61));
        assert!(!registry.uninstall(&id));
        assert_eq!(registry.retained.load(Ordering::Acquire), 0);
        registry.install_with(|| FilterKind::Block, 8).unwrap();
    }

    #[test]
    fn concurrent_installations_cannot_exceed_capacity() {
        let registry = registry(4, 4);
        let start = Arc::new(Barrier::new(17));
        let installed = std::thread::scope(|scope| {
            let tasks: Vec<_> = (0..16)
                .map(|_| {
                    let registry = registry.clone();
                    let start = start.clone();
                    scope.spawn(move || {
                        start.wait();
                        registry.install_with(|| FilterKind::Block, 0)
                    })
                })
                .collect();
            start.wait();
            tasks
                .into_iter()
                .filter_map(|task| task.join().unwrap().ok())
                .collect::<Vec<_>>()
        });
        assert_eq!(installed.len(), 4);
        assert_eq!(registry.filters.len(), 4);
        assert_eq!(registry.retained.load(Ordering::Acquire), 4);
        for id in installed {
            assert!(registry.uninstall(&id));
        }
        assert_eq!(registry.retained.load(Ordering::Acquire), 0);
    }

    #[test]
    fn replacing_an_entry_releases_its_reservation() {
        let registry = registry(2, 4);
        let id = registry.install_with(|| FilterKind::Block, 0).unwrap();
        let replacement = ActiveFilter {
            block: 1,
            last_poll_timestamp: Instant::now(),
            kind: FilterKind::Block,
            _reservation: registry.reserve().unwrap(),
        };
        drop(
            registry
                .filters
                .insert(id.clone(), Arc::new(Mutex::new(replacement))),
        );
        assert_eq!(registry.retained.load(Ordering::Acquire), 1);
        assert!(registry.uninstall(&id));
        assert_eq!(registry.retained.load(Ordering::Acquire), 0);
    }

    #[test]
    fn removing_a_pending_filter_drops_its_listener() {
        let registry = registry(1, 4);
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let id = registry
            .install_with(
                || {
                    FilterKind::PendingTransaction(PendingTransactionKind::Hashes(
                        PendingTransactionsReceiver::new(receiver),
                    ))
                },
                0,
            )
            .unwrap();
        assert!(!sender.is_closed());
        assert!(registry.uninstall(&id));
        assert!(sender.is_closed());
        assert_eq!(registry.retained.load(Ordering::Acquire), 0);
    }

    #[test]
    fn installed_log_criteria_share_one_aggregate_term_limit() {
        let registry = registry(2, 3);
        let mut filter = Filter {
            address: vec![Address::ZERO, Address::repeat_byte(1)].into(),
            ..Filter::default()
        };
        filter.topics[0] = vec![B256::ZERO].into();
        assert_eq!(registry.bounded_log_filter(filter.clone()).unwrap(), filter);
        filter.topics[1] = vec![B256::repeat_byte(1)].into();
        assert!(matches!(
            registry.bounded_log_filter(filter),
            Err(EthFilterError::FilterCriteriaTooLarge { max_terms: 3 })
        ));
        registry.bounded_log_filter(Filter::default()).unwrap();
    }

    #[test]
    fn idle_polls_renew_the_filter_lease() {
        let registry = registry(1, 4);
        let id = registry.install_with(|| FilterKind::Block, 8).unwrap();
        registry
            .filters
            .get(&id)
            .unwrap()
            .lock()
            .unwrap()
            .last_poll_timestamp = Instant::now() - Duration::from_secs(30);
        registry
            .poll(id.clone(), |start, _| Ok(((), start)))
            .unwrap();
        registry.clear_stale(Instant::now() + Duration::from_secs(31));
        assert!(registry.uninstall(&id));
    }

    #[test]
    fn an_in_flight_poll_does_not_block_eviction_or_uninstall() {
        let registry = registry(1, 4);
        let id = registry.install_with(|| FilterKind::Block, 8).unwrap();
        let entry = Arc::clone(registry.filters.get(&id).unwrap().value());
        let guard = entry.lock().unwrap();
        registry.clear_stale(Instant::now() + Duration::from_secs(61));
        assert!(registry.uninstall(&id));
        assert_eq!(registry.retained.load(Ordering::Acquire), 1);
        drop(guard);
        drop(entry);
        assert_eq!(registry.retained.load(Ordering::Acquire), 0);
    }

    #[test]
    fn failed_scans_do_not_consume_changes() {
        let registry = registry(1, 4);
        let id = registry.install_with(|| FilterKind::Block, 7).unwrap();
        let result = registry.poll::<()>(id.clone(), |_, _| {
            Err(EthFilterError::QueryExceedsMaxBlocks(1))
        });
        assert!(result.is_err());
        assert_eq!(registry.filters.get(&id).unwrap().lock().unwrap().block, 7);
        let start = registry
            .poll(id.clone(), |start, _| Ok((start, 9)))
            .unwrap();
        assert_eq!(start, 7);
        assert_eq!(registry.filters.get(&id).unwrap().lock().unwrap().block, 9);
    }

    #[test]
    fn pending_notifications_are_drained_without_new_blocks() {
        let registry = registry(1, 4);
        let (sender, receiver) = tokio::sync::mpsc::channel(2);
        let id = registry
            .install_with(
                || {
                    FilterKind::PendingTransaction(PendingTransactionKind::Hashes(
                        PendingTransactionsReceiver::new(receiver),
                    ))
                },
                8,
            )
            .unwrap();
        for expected in [B256::repeat_byte(1), B256::repeat_byte(2)] {
            sender.try_send(expected).unwrap();
            let changes = registry
                .poll(id.clone(), |start, kind| {
                    let FilterKind::PendingTransaction(pending) = kind else {
                        panic!("expected pending filter");
                    };
                    Ok((pending.drain(), start))
                })
                .unwrap();
            let alloy::rpc::types::FilterChanges::Hashes(hashes) = changes else {
                panic!("expected pending hashes");
            };
            assert_eq!(hashes, vec![expected]);
            assert_eq!(registry.filters.get(&id).unwrap().lock().unwrap().block, 8);
        }
    }

    #[test]
    fn concurrent_polls_cannot_return_the_same_cursor() {
        let registry = registry(1, 4);
        let id = registry.install_with(|| FilterKind::Block, 7).unwrap();
        let start = Arc::new(Barrier::new(9));
        let mut cursors = std::thread::scope(|scope| {
            let tasks: Vec<_> = (0..8)
                .map(|_| {
                    let registry = registry.clone();
                    let id = id.clone();
                    let start = start.clone();
                    scope.spawn(move || {
                        start.wait();
                        registry.poll(id, |cursor, _| Ok((cursor, cursor + 1)))
                    })
                })
                .collect();
            start.wait();
            tasks
                .into_iter()
                .map(|task| task.join().unwrap().unwrap())
                .collect::<Vec<_>>()
        });
        cursors.sort_unstable();
        assert_eq!(cursors, (7..15).collect::<Vec<_>>());
    }
}
