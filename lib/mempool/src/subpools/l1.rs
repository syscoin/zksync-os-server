use futures::{Stream, StreamExt};
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::{Notify, RwLock, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use zksync_os_types::{L1PriorityEnvelope, L1TxSerialId, ZkTransaction};

/// How often `pop_wait` warns while blocked on a priority transaction missing from the pool.
const EMPTY_POOL_WARN_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct L1Subpool {
    notify: Arc<Notify>,
    inner: Arc<RwLock<Inner>>,
    channel_size: usize,
}

/// New txs are added to `Inner` as well as it's used to create `L1TransactionsStream`.
/// `sender` is used to submit new transactions to the active stream.
/// If there is no active stream, then sender will be dropped on the next access; tx is inserted to `pending_txs` anyway.
struct Inner {
    sender: Option<mpsc::Sender<Arc<L1PriorityEnvelope>>>,
    pending_txs: VecDeque<Arc<L1PriorityEnvelope>>,
}

impl L1Subpool {
    pub fn new(channel_size: usize) -> Self {
        Self {
            notify: Arc::new(Notify::new()),
            inner: Arc::new(RwLock::new(Inner {
                sender: None,
                pending_txs: VecDeque::new(),
            })),
            channel_size,
        }
    }

    pub async fn best_transactions_stream(&self) -> L1TransactionsStream {
        let (sender, receiver) = mpsc::channel(self.channel_size);
        let mut inner = self.inner.write().await;
        inner.sender = Some(sender);
        L1TransactionsStream {
            receiver: ReceiverStream::new(receiver),
            pending_txs: inner.pending_txs.clone(),
            exclusive_end: None,
        }
    }

    pub(crate) async fn wait_for_priority_id(&self, expected: L1TxSerialId) -> anyhow::Result<()> {
        loop {
            let notified = self.notify.notified();
            if let Some(tx) = self.inner.read().await.pending_txs.back() {
                anyhow::ensure!(
                    tx.priority_id() == expected,
                    "oldest pending priority transaction differs from its reservation"
                );
                return Ok(());
            }
            if tokio::time::timeout(EMPTY_POOL_WARN_INTERVAL, notified)
                .await
                .is_err()
            {
                tracing::warn!(expected, "waiting for the reserved priority request");
            }
        }
    }

    pub async fn insert(&mut self, tx: Arc<L1PriorityEnvelope>) {
        let mut inner = self.inner.write().await;
        if let Some(sender) = &inner.sender {
            // If the receiver has been dropped, we should stop sending transactions and clear the sender to avoid unnecessary work.
            if sender.send(tx.clone()).await.is_err() {
                inner.sender.take();
            }
        }
        inner.pending_txs.push_front(tx);
        self.notify.notify_waiters();
    }

    async fn pop_wait(&self) -> Arc<L1PriorityEnvelope> {
        let started_at = tokio::time::Instant::now();
        loop {
            let notified = self.notify.notified();
            {
                let mut inner = self.inner.write().await;
                if let Some(pending_tx) = inner.pending_txs.pop_back() {
                    return pending_tx;
                }
            }
            // A long wait means the L1 watcher stopped delivering priority transactions
            // and the calling replay/execution pipeline is blocked on it.
            if tokio::time::timeout(EMPTY_POOL_WARN_INTERVAL, notified)
                .await
                .is_err()
            {
                tracing::warn!(
                    waited = ?started_at.elapsed(),
                    "L1 subpool has no pending priority transaction; blocked waiting for the L1 watcher to deliver it"
                );
            }
        }
    }

    pub async fn on_canonical_state_change(
        &self,
        txs: Vec<&L1PriorityEnvelope>,
    ) -> Option<L1TxSerialId> {
        if txs.is_empty() {
            return None;
        }

        let mut priority_id = 0;
        for tx in txs {
            let pending_tx = self.pop_wait().await;
            assert_eq!(tx, pending_tx.as_ref());
            priority_id = pending_tx.priority_id();
        }

        Some(priority_id)
    }
}

pub struct L1TransactionsStream {
    receiver: ReceiverStream<Arc<L1PriorityEnvelope>>,
    pending_txs: VecDeque<Arc<L1PriorityEnvelope>>,
    exclusive_end: Option<L1TxSerialId>,
}

impl L1TransactionsStream {
    pub(crate) fn with_exclusive_end(mut self, end: Option<L1TxSerialId>) -> Self {
        self.exclusive_end = end;
        self
    }
}

impl Stream for L1TransactionsStream {
    type Item = ZkTransaction;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let tx = match self.pending_txs.pop_back() {
            Some(tx) => tx,
            None => match self.receiver.poll_next_unpin(cx) {
                Poll::Ready(Some(tx)) => tx,
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            },
        };
        if self
            .exclusive_end
            .is_some_and(|end| tx.priority_id() >= end)
        {
            self.pending_txs.clear();
            self.receiver.close();
            return Poll::Ready(None);
        }
        Poll::Ready(Some(tx.as_ref().clone().into()))
    }
}
