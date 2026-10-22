use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::has_block_range_end::HasBlockRangeEnd;
use crate::metrics::PIPELINE_METRICS;

/// Report sustained backpressure without treating a slow consumer as a failed
/// consumer. Some downstream stages legitimately wait for external finality.
const STALL_WARNING_INTERVAL: Duration = Duration::from_secs(60);

/// Error returned by [`SendAndRecordExt::send_and_record`].
pub enum PipelineSendError<T> {
    /// The channel's receiver was dropped.
    Closed(T),
}

impl<T> fmt::Debug for PipelineSendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed(_) => write!(f, "PipelineSendError::Closed(..)"),
        }
    }
}

impl<T> fmt::Display for PipelineSendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed(_) => write!(
                f,
                "pipeline channel closed: downstream receiver was dropped"
            ),
        }
    }
}

impl<T> std::error::Error for PipelineSendError<T> {}

/// Extension trait on `mpsc::Sender<T>` that combines sending an item
/// with recording it as processed on a `ComponentStateReporter`.
///
/// Uses `try_send` as a zero-cost fast path. If the channel is full, records the
/// stall and waits for capacity, reporting sustained backpressure every
/// [`STALL_WARNING_INTERVAL`]. The same bounded send remains queued until it
/// succeeds, the receiver closes, or the component is cancelled. Recording
/// happens only after a successful send.
#[async_trait]
pub trait SendAndRecordExt<T: HasBlockRangeEnd> {
    async fn send_and_record(
        &self,
        value: T,
        reporter: &zksync_os_observability::ComponentStateReporter,
    ) -> Result<(), PipelineSendError<T>>;
}

#[async_trait]
impl<T: HasBlockRangeEnd> SendAndRecordExt<T> for mpsc::Sender<T> {
    async fn send_and_record(
        &self,
        value: T,
        reporter: &zksync_os_observability::ComponentStateReporter,
    ) -> Result<(), PipelineSendError<T>> {
        let block_number = value.block_number();
        let block_timestamp = value.block_timestamp();
        let batch_number = value.batch_number();

        match self.try_send(value) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Closed(v)) => {
                return Err(PipelineSendError::Closed(v));
            }
            Err(mpsc::error::TrySendError::Full(value)) => {
                let component = reporter.component_name();
                tracing::info!(
                    component,
                    "pipeline channel stall: output channel is full; downstream consumer is behind"
                );
                PIPELINE_METRICS.channel_stall_count[&component].inc();
                let waiting_since = tokio::time::Instant::now();
                let warnings = tokio::time::interval_at(
                    waiting_since + STALL_WARNING_INTERVAL,
                    STALL_WARNING_INTERVAL,
                );
                tokio::pin!(warnings);
                warnings.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                // Keep one send future alive across warnings. Recreating it
                // would lose its position in Tokio's capacity queue.
                let send = self.send(value);
                tokio::pin!(send);
                loop {
                    tokio::select! {
                        biased;
                        result = &mut send => {
                            result.map_err(|mpsc::error::SendError(v)| PipelineSendError::Closed(v))?;
                            break;
                        }
                        _ = warnings.tick() => {
                            tracing::warn!(
                                component,
                                elapsed_seconds = waiting_since.elapsed().as_secs(),
                                "pipeline backpressure: still waiting for downstream capacity"
                            );
                        }
                    }
                }
            }
        }

        reporter.record_processed(block_number, block_timestamp, batch_number);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::poll;
    use std::task::Poll;
    use zksync_os_observability::ComponentStateReporter;

    #[derive(Debug, PartialEq, Eq)]
    struct Item(u64);

    impl HasBlockRangeEnd for Item {
        fn block_number(&self) -> u64 {
            self.0
        }

        fn block_timestamp(&self) -> Option<u64> {
            Some(self.0 + 100)
        }

        fn batch_number(&self) -> Option<u64> {
            Some(self.0 + 10)
        }
    }

    #[tokio::test(start_paused = true)]
    async fn sustained_backpressure_preserves_fifo_and_records_only_completed_sends() {
        let (sender, mut receiver) = mpsc::channel(1);
        sender.send(Item(1)).await.unwrap();
        let (reporter, state) = ComponentStateReporter::new("backpressure_fifo_test");
        let (later_reporter, later_state) = ComponentStateReporter::new("backpressure_later_test");
        let mut first = Box::pin(sender.send_and_record(Item(2), &reporter));
        let mut later = Box::pin(sender.send_and_record(Item(3), &later_reporter));
        assert!(poll!(first.as_mut()).is_pending());
        assert!(poll!(later.as_mut()).is_pending());
        for _ in 0..2 {
            tokio::time::advance(Duration::from_secs(61)).await;
            assert!(poll!(first.as_mut()).is_pending());
            assert!(poll!(later.as_mut()).is_pending());
            assert!(state.borrow().processed.is_none());
            assert!(later_state.borrow().processed.is_none());
        }

        assert_eq!(receiver.recv().await, Some(Item(1)));
        assert!(matches!(poll!(later.as_mut()), Poll::Pending));
        assert!(matches!(poll!(first.as_mut()), Poll::Ready(Ok(()))));
        let processed = state.borrow().processed.clone().unwrap();
        assert_eq!(processed.block_number, 2);
        assert_eq!(processed.timestamp, Some(102));
        assert_eq!(processed.batch_number, Some(12));
        assert_eq!(receiver.recv().await, Some(Item(2)));
        assert!(matches!(poll!(later.as_mut()), Poll::Ready(Ok(()))));
        assert_eq!(receiver.recv().await, Some(Item(3)));
    }

    #[tokio::test(start_paused = true)]
    async fn downstream_finality_wait_backpressures_multiple_stages_without_restarting() {
        let (upstream, mut middle) = mpsc::channel(1);
        let (downstream, mut finality_gate) = mpsc::channel(1);
        downstream.send(Item(0)).await.unwrap();
        let (producer_reporter, producer_state) =
            ComponentStateReporter::new("backpressure_producer_test");
        let (relay_reporter, relay_state) = ComponentStateReporter::new("backpressure_relay_test");
        let producer = tokio::spawn(async move {
            for number in 1..=6 {
                upstream
                    .send_and_record(Item(number), &producer_reporter)
                    .await
                    .unwrap();
            }
        });
        let relay = tokio::spawn(async move {
            while let Some(item) = middle.recv().await {
                downstream
                    .send_and_record(item, &relay_reporter)
                    .await
                    .unwrap();
            }
        });
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            producer_state
                .borrow()
                .processed
                .as_ref()
                .unwrap()
                .block_number,
            2
        );
        assert!(relay_state.borrow().processed.is_none());
        for _ in 0..2 {
            tokio::time::advance(Duration::from_secs(61)).await;
            tokio::task::yield_now().await;
            assert!(!producer.is_finished());
            assert!(!relay.is_finished());
            assert_eq!(
                producer_state
                    .borrow()
                    .processed
                    .as_ref()
                    .unwrap()
                    .block_number,
                2
            );
            assert!(relay_state.borrow().processed.is_none());
        }
        for number in 0..=6 {
            assert_eq!(finality_gate.recv().await, Some(Item(number)));
        }
        producer.await.unwrap();
        relay.await.unwrap();
        assert_eq!(
            producer_state
                .borrow()
                .processed
                .as_ref()
                .unwrap()
                .block_number,
            6
        );
        assert_eq!(
            relay_state
                .borrow()
                .processed
                .as_ref()
                .unwrap()
                .block_number,
            6
        );
    }

    #[tokio::test(start_paused = true)]
    async fn receiver_closed_before_send_returns_unsent_item_without_recording() {
        let (sender, receiver) = mpsc::channel(1);
        drop(receiver);
        let (reporter, state) = ComponentStateReporter::new("backpressure_closed_test");
        assert!(matches!(
            sender.send_and_record(Item(4), &reporter).await,
            Err(PipelineSendError::Closed(Item(4)))
        ));
        assert!(state.borrow().processed.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn receiver_closed_while_waiting_returns_unsent_item_without_recording() {
        let (sender, receiver) = mpsc::channel(1);
        sender.send(Item(1)).await.unwrap();
        let (reporter, state) = ComponentStateReporter::new("backpressure_close_wait_test");
        let mut send = Box::pin(sender.send_and_record(Item(4), &reporter));
        assert!(poll!(send.as_mut()).is_pending());
        tokio::time::advance(Duration::from_secs(121)).await;
        assert!(poll!(send.as_mut()).is_pending());
        drop(receiver);
        assert!(matches!(
            send.await,
            Err(PipelineSendError::Closed(Item(4)))
        ));
        assert!(state.borrow().processed.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_drops_pending_item_without_recording_or_sending_it() {
        let (sender, mut receiver) = mpsc::channel(1);
        sender.send(Item(1)).await.unwrap();
        let (reporter, state) = ComponentStateReporter::new("backpressure_cancel_test");
        let mut send = Box::pin(sender.send_and_record(Item(4), &reporter));
        assert!(poll!(send.as_mut()).is_pending());
        tokio::time::advance(Duration::from_secs(121)).await;
        assert!(poll!(send.as_mut()).is_pending());
        drop(send);
        assert!(state.borrow().processed.is_none());
        assert_eq!(receiver.recv().await, Some(Item(1)));
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        sender.send_and_record(Item(5), &reporter).await.unwrap();
        assert_eq!(receiver.recv().await, Some(Item(5)));
        assert_eq!(state.borrow().processed.as_ref().unwrap().block_number, 5);
    }
}
