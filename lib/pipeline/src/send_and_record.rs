use std::fmt;

use crate::has_block_range_end::HasBlockRangeEnd;
use async_trait::async_trait;
use tokio::sync::mpsc;

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
            Self::Closed(_) => write!(f, "pipeline channel closed: receiver was dropped"),
        }
    }
}

impl<T> std::error::Error for PipelineSendError<T> {}

/// Extension trait on `mpsc::Sender<T>` that combines sending an item
/// with recording it as processed on a `ComponentStateReporter`.
///
/// Recording happens only if the send succeeds — if the receiver has been
/// dropped, the error is returned and nothing is recorded.
#[async_trait]
pub trait SendAndRecordExt<T: HasBlockRangeEnd + Send> {
    async fn send_and_record(
        &self,
        value: T,
        reporter: &zksync_os_observability::ComponentStateReporter,
    ) -> Result<(), PipelineSendError<T>>;
}

#[async_trait]
impl<T: HasBlockRangeEnd + Send> SendAndRecordExt<T> for mpsc::Sender<T> {
    async fn send_and_record(
        &self,
        value: T,
        reporter: &zksync_os_observability::ComponentStateReporter,
    ) -> Result<(), PipelineSendError<T>> {
        let block_number = value.block_number();
        let block_timestamp = value.block_timestamp();
        let batch_number = value.batch_number();
        if let Err(err) = self.send(value).await {
            return Err(PipelineSendError::Closed(err.0));
        }
        reporter.record_processed(block_number, block_timestamp, batch_number);
        Ok(())
    }
}
