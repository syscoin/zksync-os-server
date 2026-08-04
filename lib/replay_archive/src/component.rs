use crate::metrics::REPLAY_ARCHIVE_METRICS;
use crate::replay_record::encode_replay_record;
use crate::{REPLAY_ARCHIVE_QUEUE_SIZE, ReplayArchiver};
use alloy::primitives::{B256, BlockHash, BlockNumber, keccak256};
use anyhow::Context as _;
use futures::{StreamExt as _, TryStreamExt as _};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use zksync_os_storage_api::ReplayRecord;

pub type ReplayArchiveRecord = (BlockHash, ReplayRecord);
pub type ReplayArchiveSender = mpsc::Sender<ReplayArchiveRecord>;

const MAX_PARALLEL_OBJECT_PUTS: usize = 10;

/// Background component that archives replay records from a bounded queue.
///
/// The block applier only waits until a record is accepted into this component's bounded queue. The
/// actual archive write happens here, off the block-application path. If this queue is full,
/// senders apply backpressure until the component catches up.
pub struct ReplayArchiveComponent<Archive> {
    archive: Archive,
    records: mpsc::Receiver<ReplayArchiveRecord>,
}

impl<Archive> ReplayArchiveComponent<Archive>
where
    Archive: ReplayArchiver,
{
    pub fn new(archive: Archive) -> (ReplayArchiveSender, Self) {
        let (sender, records) = mpsc::channel(REPLAY_ARCHIVE_QUEUE_SIZE);
        (sender, Self { archive, records })
    }

    pub async fn run(self) -> anyhow::Result<()> {
        let Self { archive, records } = self;
        let highest_archived_block_number = Mutex::new(None);
        let recent_records = Mutex::new(RecentReplayRecords::default());

        ReceiverStream::new(records)
            .map(Ok::<_, anyhow::Error>)
            .try_for_each_concurrent(MAX_PARALLEL_OBJECT_PUTS, |record| {
                let archive = &archive;
                let highest_archived_block_number = &highest_archived_block_number;
                let recent_records = &recent_records;

                async move {
                    let fingerprint = ReplayRecordFingerprint::new(&record);
                    let should_archive = recent_records
                        .lock()
                        .expect("recent replay record mutex is poisoned")
                        .register(fingerprint)?;
                    if !should_archive {
                        tracing::debug!(
                            block_number = fingerprint.block_number,
                            block_hash = %fingerprint.block_hash,
                            "Skipping duplicate replay archive enqueue"
                        );
                        return Ok(());
                    }

                    let archived_block_number = archive_replay_record(archive, record).await?;
                    recent_records
                        .lock()
                        .expect("recent replay record mutex is poisoned")
                        .mark_completed(fingerprint);
                    update_highest_archived_block_number(
                        highest_archived_block_number,
                        archived_block_number,
                    );
                    Ok(())
                }
            })
            .await
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReplayRecordFingerprint {
    block_number: BlockNumber,
    block_hash: BlockHash,
    payload_hash: B256,
}

impl ReplayRecordFingerprint {
    fn new((block_hash, replay_record): &ReplayArchiveRecord) -> Self {
        Self {
            block_number: replay_record.block_context.block_number,
            block_hash: *block_hash,
            payload_hash: keccak256(encode_replay_record(replay_record)),
        }
    }

    fn key(self) -> (BlockNumber, BlockHash) {
        (self.block_number, self.block_hash)
    }
}

#[derive(Debug, Default)]
struct RecentReplayRecords {
    in_flight: HashMap<(BlockNumber, BlockHash), B256>,
    completed: HashMap<(BlockNumber, BlockHash), B256>,
    completed_order: VecDeque<(BlockNumber, BlockHash)>,
}

impl RecentReplayRecords {
    fn register(&mut self, fingerprint: ReplayRecordFingerprint) -> anyhow::Result<bool> {
        let key = fingerprint.key();
        if let Some(existing_hash) = self
            .in_flight
            .get(&key)
            .or_else(|| self.completed.get(&key))
        {
            anyhow::ensure!(
                *existing_hash == fingerprint.payload_hash,
                "conflicting replay records queued for block #{}, {}",
                fingerprint.block_number,
                fingerprint.block_hash
            );
            return Ok(false);
        }

        // SYSCOIN: WAL replay must populate a fresh session even when local replay storage reports
        // an existing record. Keep identical retry enqueues away from fail-closed storage without
        // making an object created outside this component count as a successful append.
        self.in_flight.insert(key, fingerprint.payload_hash);
        Ok(true)
    }

    fn mark_completed(&mut self, fingerprint: ReplayRecordFingerprint) {
        let key = fingerprint.key();
        let payload_hash = self
            .in_flight
            .remove(&key)
            .expect("completed replay record must be in flight");
        self.completed.insert(key, payload_hash);
        self.completed_order.push_back(key);

        while self.completed_order.len() > REPLAY_ARCHIVE_QUEUE_SIZE {
            let oldest = self
                .completed_order
                .pop_front()
                .expect("completed replay record queue must not be empty");
            self.completed.remove(&oldest);
        }
    }
}

fn update_highest_archived_block_number(
    highest_archived_block_number: &Mutex<Option<BlockNumber>>,
    archived_block_number: BlockNumber,
) {
    let mut highest_archived_block_number = highest_archived_block_number
        .lock()
        .expect("highest archived block number mutex is poisoned");

    if highest_archived_block_number.is_none_or(|highest| archived_block_number > highest) {
        REPLAY_ARCHIVE_METRICS
            .last_archived_block_number
            .set(archived_block_number);
        *highest_archived_block_number = Some(archived_block_number);
    }
}

async fn archive_replay_record<Archive>(
    archive: &Archive,
    (block_hash, replay_record): ReplayArchiveRecord,
) -> anyhow::Result<BlockNumber>
where
    Archive: ReplayArchiver,
{
    let block_number = replay_record.block_context.block_number;
    tracing::info!("Archiving replay record for block #{block_number}, {block_hash}");
    let archive_time = REPLAY_ARCHIVE_METRICS.archive_time.start();
    archive
        .append_replay_record(block_hash, replay_record)
        .await
        .with_context(|| format!("failed to archive replay record for block {block_number}"))?;
    archive_time.observe();
    Ok(block_number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::B256;
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use vise::{Format, MetricsCollection};

    #[derive(Debug, Default)]
    struct RecordingArchiver {
        appended: AtomicUsize,
    }

    #[async_trait]
    impl ReplayArchiver for RecordingArchiver {
        async fn append_replay_record(
            &self,
            _block_hash: BlockHash,
            _replay_record: ReplayRecord,
        ) -> anyhow::Result<()> {
            self.appended.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn contains_replay_record(
            &self,
            _block_number: BlockNumber,
            _block_hash: BlockHash,
        ) -> anyhow::Result<bool> {
            Ok(false)
        }
    }

    // Relies on nextest's process-per-test isolation: the global metric is not shared with
    // other tests.
    #[tokio::test]
    async fn archiving_observes_archive_time_metric() {
        let archiver = Arc::new(RecordingArchiver::default());
        let (sender, component) = ReplayArchiveComponent::new(archiver.clone());

        sender
            .send((B256::with_last_byte(1), test_replay_record()))
            .await
            .unwrap();
        drop(sender);
        component.run().await.unwrap();

        assert_eq!(archiver.appended.load(Ordering::SeqCst), 1);
        let mut encoded = String::new();
        MetricsCollection::default()
            .collect()
            .encode(&mut encoded, Format::OpenMetrics)
            .unwrap();
        let count_line = encoded
            .lines()
            .find(|line| line.starts_with("replay_archive_archive_time_seconds_count"))
            .unwrap_or_else(|| panic!("archive_time metric is not exported:\n{encoded}"));
        assert!(count_line.ends_with(" 1"), "{count_line}");
    }

    #[test]
    fn identical_enqueues_are_deduplicated() {
        let mut recent = RecentReplayRecords::default();
        let record = (B256::with_last_byte(1), test_replay_record());
        let fingerprint = ReplayRecordFingerprint::new(&record);

        assert!(recent.register(fingerprint).unwrap());
        assert!(!recent.register(fingerprint).unwrap());
        recent.mark_completed(fingerprint);
        assert!(!recent.register(fingerprint).unwrap());
    }

    #[test]
    fn conflicting_enqueues_fail_closed() {
        let mut recent = RecentReplayRecords::default();
        let block_hash = B256::with_last_byte(1);
        let first = test_replay_record();
        let mut conflicting = first.clone();
        conflicting.block_output_hash = B256::with_last_byte(2);

        recent
            .register(ReplayRecordFingerprint::new(&(block_hash, first)))
            .unwrap();
        let err = recent
            .register(ReplayRecordFingerprint::new(&(block_hash, conflicting)))
            .unwrap_err();

        assert!(
            err.to_string()
                .contains("conflicting replay records queued"),
            "{err:#}"
        );
    }

    fn test_replay_record() -> ReplayRecord {
        ReplayRecord {
            block_context: zksync_os_storage_api::BlockContext {
                block_number: 7,
                ..Default::default()
            },
            transactions: vec![],
            previous_block_timestamp: 0,
            node_version: "0.0.0".parse().unwrap(),
            protocol_version: "0.29.1".parse().unwrap(),
            block_output_hash: B256::ZERO,
            force_preimages: vec![],
            canonical_upgrade_tx_hash: B256::ZERO,
            starting_cursors: Default::default(),
        }
    }
}
