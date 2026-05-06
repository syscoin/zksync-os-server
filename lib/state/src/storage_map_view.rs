use crate::metrics::STORAGE_VIEW_METRICS;
use crate::{Diff, PersistentStorageMap};
use alloy::primitives::B256;
use dashmap::DashMap;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use zksync_os_interface::traits::ReadStorage;

// SYSCOIN: Keep track of live historical views so background compaction never
// advances the persistent base past a view's target block while it can still be read.
#[derive(Debug, Clone, Default)]
pub(crate) struct ActiveStorageViews {
    blocks: Arc<Mutex<BTreeMap<u64, usize>>>,
}

impl ActiveStorageViews {
    pub(crate) fn lock(&self) -> MutexGuard<'_, BTreeMap<u64, usize>> {
        self.blocks
            .lock()
            .expect("active storage views lock poisoned")
    }

    pub(crate) fn oldest_locked(&self, blocks: &BTreeMap<u64, usize>) -> Option<u64> {
        blocks.first_key_value().map(|(block, _count)| *block)
    }

    pub(crate) fn register_locked(
        &self,
        block: u64,
        blocks: &mut BTreeMap<u64, usize>,
    ) -> ActiveStorageViewGuard {
        *blocks.entry(block).or_default() += 1;
        ActiveStorageViewGuard {
            block,
            active_views: Some(self.clone()),
        }
    }

    fn unregister_locked(block: u64, blocks: &mut BTreeMap<u64, usize>) {
        let count = blocks
            .get_mut(&block)
            .expect("active storage view guard must be registered");
        *count -= 1;
        if *count == 0 {
            blocks.remove(&block);
        }
    }
}

#[derive(Debug)]
pub(crate) struct ActiveStorageViewGuard {
    block: u64,
    active_views: Option<ActiveStorageViews>,
}

impl ActiveStorageViewGuard {
    pub(crate) fn unregister_with_lock(&mut self, blocks: &mut BTreeMap<u64, usize>) {
        if self.active_views.take().is_some() {
            ActiveStorageViews::unregister_locked(self.block, blocks);
        }
    }
}

impl Clone for ActiveStorageViewGuard {
    fn clone(&self) -> Self {
        let active_views = self
            .active_views
            .as_ref()
            .expect("active storage view guard must be registered")
            .clone();
        let mut blocks = active_views.lock();
        *blocks.entry(self.block).or_default() += 1;
        drop(blocks);

        Self {
            block: self.block,
            active_views: Some(active_views),
        }
    }
}

impl Drop for ActiveStorageViewGuard {
    fn drop(&mut self) {
        if let Some(active_views) = self.active_views.take() {
            let mut blocks = active_views.lock();
            ActiveStorageViews::unregister_locked(self.block, &mut blocks);
        }
    }
}
// SYSCOIN

/// Storage View valid for a specific block (`block`)
/// It represents the state immediately after block `block`.
#[derive(Debug, Clone)]
pub struct StorageMapView {
    /// Block number for which this view is valid.
    pub block: u64,
    /// Block preceding the first block in diffs
    /// note: it's possible that persistence will be compacted for blocks after `base_block`
    /// and diffs removed from memory.
    /// SYSCOIN: The active-view guard prevents compaction from moving past `block`,
    /// so fallback persistence never represents a future state for this view.
    // todo: in fact we could infer this from `diffs` - by iterating backwards until the first missing element
    pub base_block: u64,
    /// All diffs after `base_block` and before `block`
    pub diffs: Arc<DashMap<u64, Arc<Diff>>>,
    /// fallback persistence for cases when value is not in diffs
    pub persistent_storage_map: PersistentStorageMap,
    // SYSCOIN: Drops this view from the active-view set so compaction can move
    // past `block` only after all clones of this view are gone.
    pub(crate) _active_view_guard: ActiveStorageViewGuard,
}

impl ReadStorage for StorageMapView {
    /// Reads `key` by scanning block diffs from `block` down to `base_block + 1`,
    /// then falling back to the persistence
    fn read(&mut self, key: B256) -> Option<B256> {
        let diffs_latency_observer = STORAGE_VIEW_METRICS.access[&"diff"].start();
        let total_latency_observer = STORAGE_VIEW_METRICS.access[&"total"].start();

        for bn in (self.base_block + 1..=self.block).rev() {
            if let Some(diff) = self.diffs.get(&bn) {
                let res = diff.map.get(&key);
                if let Some(value) = res {
                    diffs_latency_observer.observe();
                    total_latency_observer.observe();
                    STORAGE_VIEW_METRICS.diffs_scanned.observe(self.block - bn);
                    return Some(*value);
                }
            } else {
                tracing::debug!(
                    "StorageMapView for {} (base block {}) read key: no diff found for block {}",
                    self.block,
                    self.base_block,
                    bn
                );
                // SYSCOIN: The active-view guard ensures compaction can only
                // remove diffs up to this view's block. Falling back to the
                // persistent base is safe because it cannot contain future
                // state while this view is alive.
                break;
            }
        }

        diffs_latency_observer.observe();
        STORAGE_VIEW_METRICS
            .diffs_scanned
            .observe(self.block - self.base_block);

        // Fallback to base_state
        let base_latency_observer = STORAGE_VIEW_METRICS.access[&"base"].start();
        let r = self.persistent_storage_map.get(key);
        base_latency_observer.observe();

        total_latency_observer.observe();
        r
    }
}
