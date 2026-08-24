use alloy::consensus::Transaction;
use anyhow::Context;
use futures::stream::BoxStream;
use std::collections::{BTreeMap, VecDeque};
use std::num::NonZeroUsize;
use std::sync::{Arc, RwLock};
use tokio::sync::Notify;
use tokio::time::Instant;
use tokio::time::sleep_until;
use zksync_os_types::{
    IndexedInteropRoot, InteropRoot, SystemTxEnvelope, SystemTxType, ZkTransaction,
};

#[derive(Clone)]
pub struct InteropRootsSubpool {
    /// Consistent state of pending roots shared between all clones of this subpool.
    inner: Arc<RwLock<Inner>>,
    notify: Arc<Notify>,
    // SYSCOIN: A nonempty root buffer must always make progress when it emits a system tx.
    interop_roots_per_tx: NonZeroUsize,
}

/// Holds all **pending** interop roots, i.e. those that have been received but not included in the
/// canonical chain yet. Note that some prefix might have already been executed in sequencer (as
/// they were returned from [`InteropRootsSubpool::interop_transactions_with_delay`]).
struct Inner {
    pending_roots: BTreeMap<u64, InteropRoot>,
    // SYSCOIN: Exact next source ID expected by canonical production / replay cleanup. V32's
    // MessageRoot starts IDs at one; the persisted zero cursor is only the fresh-chain sentinel.
    next_interop_root_id: Option<u64>,
}

impl InteropRootsSubpool {
    pub fn new(interop_roots_per_tx: NonZeroUsize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner {
                pending_roots: BTreeMap::new(),
                next_interop_root_id: None,
            })),
            notify: Arc::new(Notify::new()),
            interop_roots_per_tx,
        }
    }

    // SYSCOIN: Seed the replay-derived V32 cursor exactly once; double initialization is a startup
    // topology error rather than permission to discard or repeat interop roots.
    pub fn init(&self, starting_interop_root_id: u64) {
        let mut inner = self.inner.write().unwrap();
        assert!(
            inner.next_interop_root_id.is_none(),
            "InteropRootsSubpool is already initialized"
        );
        // SYSCOIN: `0` means "scan from genesis"; the pinned V32 contract's first emitted ID is 1.
        inner.next_interop_root_id = Some(starting_interop_root_id.max(1));
    }

    pub async fn interop_transactions_with_delay(
        &self,
        next_tx_allowed_after: Instant,
    ) -> BoxStream<'_, ZkTransaction> {
        Box::pin(futures::stream::unfold(
            (
                self.inner.clone(),
                self.notify.clone(),
                0u64,
                VecDeque::<(u64, InteropRoot)>::default(),
            ),
            move |(inner, notify, mut cursor, mut buffer)| async move {
                sleep_until(next_tx_allowed_after).await;
                loop {
                    // Subscribe BEFORE reading — avoids the race where an insert
                    // happens between our read and our .notified().await.
                    let notified = notify.notified();

                    {
                        let inner = inner.read().unwrap();
                        for (id, root) in inner.pending_roots.range(cursor..) {
                            // SYSCOIN: Never wrap the stream cursor back to genesis on malformed
                            // source IDs; strict canonical cleanup will reject the terminal ID too.
                            cursor = id
                                .checked_add(1)
                                .expect("interop root stream cursor overflow");
                            buffer.push_front((*id, root.clone()));
                        }
                    }

                    if !buffer.is_empty() {
                        let amount_of_roots_to_take =
                            buffer.len().min(self.interop_roots_per_tx.get());
                        let starting_index = buffer.len() - amount_of_roots_to_take;

                        let roots_to_consume: Vec<(u64, InteropRoot)> = buffer
                            .drain(starting_index..)
                            .rev() // reversing iterator as last element is the one received earliest
                            .collect();

                        // Use the log_id of the last (largest) root as the salt for uniqueness.
                        let last_log_id = roots_to_consume
                            .last()
                            .expect("roots_to_consume is non-empty")
                            .0;
                        let roots = roots_to_consume.into_iter().map(|(_, r)| r).collect();
                        let envelope = SystemTxEnvelope::import_interop_roots(roots, last_log_id);
                        drop(notified);
                        return Some((envelope.into(), (inner, notify, cursor, buffer)));
                    }

                    // Nothing new yet — wait for an insert, then retry.
                    notified.await;
                }
            },
        ))
    }

    pub async fn add_root(&mut self, root: IndexedInteropRoot) {
        let mut inner = self.inner.write().unwrap();
        let next_interop_root_id = inner
            .next_interop_root_id
            .expect("InteropRootsSubpool is not initialized");
        // SYSCOIN: A watcher may finish an already-started poll after canonical replay advanced the
        // cursor. Never re-queue an ID that the canonical chain has already consumed.
        if root.log_id < next_interop_root_id {
            return;
        }
        inner.pending_roots.insert(root.log_id, root.root);
        drop(inner);
        self.notify.notify_waiters();
    }

    // SYSCOIN: Live production consumes an exact contiguous source sequence and fails closed on a
    // watcher gap instead of constructing a system transaction over silently skipped roots.
    async fn pop_wait(&self) -> anyhow::Result<(u64, InteropRoot)> {
        loop {
            let notified = self.notify.notified();
            {
                let mut inner = self.inner.write().unwrap();
                if let Some((&id, _)) = inner.pending_roots.first_key_value() {
                    let expected_id = inner
                        .next_interop_root_id
                        .expect("InteropRootsSubpool is not initialized");
                    anyhow::ensure!(
                        id == expected_id,
                        "next queued interop root ID {id} does not match expected canonical ID {expected_id}"
                    );
                    let next_id = expected_id
                        .checked_add(1)
                        .context("interop root ID overflow while advancing strict cleanup")?;
                    let (_, root) = inner
                        .pending_roots
                        .pop_first()
                        .expect("first_key_value proved the map is non-empty");
                    inner.next_interop_root_id = Some(next_id);
                    return Ok((id, root));
                }
            }
            notified.await;
        }
    }

    // SYSCOIN: Replay/rebuild already authenticates the canonical system transaction through VM
    // execution and the expected block-output hash. Advance from its committed salt without waiting
    // for a watcher that intentionally does not exist after a Gateway→L1 return. Any roots that a
    // live Gateway watcher already supplied must still agree exactly with the canonical payload.
    fn consume_replayed_tx(&self, tx: &SystemTxEnvelope) -> anyhow::Result<u64> {
        let roots = tx
            .interop_roots()
            .context("tried to replay a non-interop system tx in InteropRootsSubpool")?;
        anyhow::ensure!(
            !roots.is_empty(),
            "canonical interop-root transaction must contain at least one root"
        );
        let roots_count = u64::try_from(roots.len())
            .context("canonical interop-root transaction root count does not fit in u64")?;
        let latest_log_id = tx.nonce();

        let mut inner = self.inner.write().unwrap();
        let first_log_id = inner
            .next_interop_root_id
            .expect("InteropRootsSubpool is not initialized");
        let expected_latest_log_id = first_log_id
            .checked_add(roots_count - 1)
            .context("interop root ID overflow while validating canonical replay")?;
        anyhow::ensure!(
            latest_log_id == expected_latest_log_id,
            "canonical interop-root transaction salt {latest_log_id} does not match contiguous IDs {first_log_id}..={expected_latest_log_id}"
        );

        for (offset, canonical_root) in roots.iter().enumerate() {
            let offset = u64::try_from(offset)
                .context("canonical interop-root offset does not fit in u64")?;
            let id = first_log_id
                .checked_add(offset)
                .context("interop root ID overflow while comparing canonical replay")?;
            if let Some(watched_root) = inner.pending_roots.get(&id) {
                anyhow::ensure!(
                    watched_root.chainId == canonical_root.chainId
                        && watched_root.blockOrBatchNumber == canonical_root.blockOrBatchNumber
                        && watched_root.sides == canonical_root.sides,
                    "watcher interop root ID {id} disagrees with canonical replay transaction"
                );
            }
        }

        let next_id = latest_log_id
            .checked_add(1)
            .context("interop root ID overflow while advancing canonical replay")?;
        inner.pending_roots.retain(|id, _| *id >= next_id);
        inner.next_interop_root_id = Some(next_id);
        Ok(latest_log_id)
    }

    /// Cleans up the stream and removes all roots that were sent in transactions.
    /// Returns the last log_id of the executed interop root.
    ///
    /// SYSCOIN: Strict live production waits for and compares the watcher queue; historical
    /// replay/rebuild validates the canonical payload and salt without requiring a live source.
    pub async fn on_canonical_state_change(
        &self,
        txs: Vec<&SystemTxEnvelope>,
        strict_subpool_cleanup: bool,
    ) -> anyhow::Result<Option<u64>> {
        if txs.is_empty() {
            return Ok(None);
        }

        if !strict_subpool_cleanup {
            let mut last_log_id = None;
            for tx in txs {
                last_log_id = Some(self.consume_replayed_tx(tx)?);
            }
            return Ok(last_log_id);
        }

        let mut last_log_id = None;

        for tx in txs {
            let roots_count = match *tx.system_subtype() {
                SystemTxType::ImportInteropRoots(roots_count) => roots_count,
                ref other => anyhow::bail!(
                    "tried to strictly clean unrelated system tx {other:?} in InteropRootsSubpool"
                ),
            };
            anyhow::ensure!(
                roots_count > 0,
                "canonical interop-root transaction must contain at least one root"
            );

            let roots_capacity = usize::try_from(roots_count)
                .context("strict interop-root transaction count does not fit in usize")?;
            let mut roots = Vec::with_capacity(roots_capacity);
            let mut tx_last_log_id = None;
            for _ in 0..roots_count {
                let (id, root) = self.pop_wait().await?;
                roots.push(root);
                tx_last_log_id = Some(id);
            }
            last_log_id = tx_last_log_id;
            let envelope = SystemTxEnvelope::import_interop_roots(
                roots,
                tx_last_log_id.expect("roots_count > 0"),
            );

            anyhow::ensure!(
                &envelope == tx,
                "strict interop-root cleanup disagrees with the produced canonical transaction"
            );
        }

        Ok(last_log_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{B256, U256};
    use std::time::Duration;

    fn root(marker: u8) -> InteropRoot {
        InteropRoot {
            chainId: U256::from(506),
            blockOrBatchNumber: U256::from(marker),
            sides: vec![B256::repeat_byte(marker)],
        }
    }

    fn new_subpool(starting_id: u64) -> InteropRootsSubpool {
        let subpool = InteropRootsSubpool::new(NonZeroUsize::new(100).unwrap());
        subpool.init(starting_id);
        subpool
    }

    #[tokio::test]
    async fn non_strict_historical_replay_advances_without_a_watcher() {
        let mut subpool = new_subpool(0);
        let tx = SystemTxEnvelope::import_interop_roots(vec![root(1), root(2)], 2);

        let last_id = tokio::time::timeout(
            Duration::from_millis(100),
            subpool.on_canonical_state_change(vec![&tx], false),
        )
        .await
        .expect("non-strict replay must never wait for a watcher")
        .unwrap();
        assert_eq!(last_id, Some(2));

        subpool
            .add_root(IndexedInteropRoot {
                log_id: 2,
                root: root(2),
            })
            .await;
        subpool
            .add_root(IndexedInteropRoot {
                log_id: 3,
                root: root(3),
            })
            .await;
        let inner = subpool.inner.read().unwrap();
        assert_eq!(inner.next_interop_root_id, Some(3));
        assert_eq!(
            inner.pending_roots.keys().copied().collect::<Vec<_>>(),
            vec![3]
        );
    }

    #[tokio::test]
    async fn non_strict_replay_rejects_non_contiguous_salt_and_watcher_conflicts() {
        let subpool = new_subpool(5);
        let skipped = SystemTxEnvelope::import_interop_roots(vec![root(5)], 6);
        assert!(
            subpool
                .on_canonical_state_change(vec![&skipped], false)
                .await
                .unwrap_err()
                .to_string()
                .contains("does not match contiguous IDs")
        );

        let mut subpool = new_subpool(5);
        subpool
            .add_root(IndexedInteropRoot {
                log_id: 5,
                root: root(99),
            })
            .await;
        let canonical = SystemTxEnvelope::import_interop_roots(vec![root(5)], 5);
        assert!(
            subpool
                .on_canonical_state_change(vec![&canonical], false)
                .await
                .unwrap_err()
                .to_string()
                .contains("disagrees with canonical replay")
        );
    }

    #[tokio::test]
    async fn canonical_replay_rejects_zero_roots_and_cursor_overflow() {
        let subpool = new_subpool(1);
        let empty = SystemTxEnvelope::import_interop_roots(Vec::new(), 0);
        for strict in [false, true] {
            assert!(
                subpool
                    .on_canonical_state_change(vec![&empty], strict)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("at least one root")
            );
        }

        let subpool = new_subpool(u64::MAX);
        let terminal = SystemTxEnvelope::import_interop_roots(vec![root(1)], u64::MAX);
        assert!(
            subpool
                .on_canonical_state_change(vec![&terminal], false)
                .await
                .unwrap_err()
                .to_string()
                .contains("overflow while advancing canonical replay")
        );
    }

    #[tokio::test]
    async fn strict_cleanup_still_requires_exact_queued_roots() {
        let mut subpool = new_subpool(1);
        for id in 1..=2 {
            subpool
                .add_root(IndexedInteropRoot {
                    log_id: id,
                    root: root(id as u8),
                })
                .await;
        }
        let tx = SystemTxEnvelope::import_interop_roots(vec![root(1), root(2)], 2);
        assert_eq!(
            subpool
                .on_canonical_state_change(vec![&tx], true)
                .await
                .unwrap(),
            Some(2)
        );
    }
}
