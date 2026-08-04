use crate::ReplayArchiveSender;
use crate::metrics::REPLAY_ARCHIVE_METRICS;
use alloy::primitives::{BlockHash, BlockNumber, Sealed};
use anyhow::Context;
use std::fmt::Debug;
use std::time::Instant;
use zksync_os_storage_api::{BlockContext, ReadReplay, ReplayRecord, WriteReplay};

/// [`WriteReplay`] wrapper that writes to replay storage and enqueues records for archiving.
#[derive(Debug, Clone)]
pub struct ReplayArchivingWriteReplay<Replay> {
    replay: Replay,
    archive_sender: Option<ReplayArchiveSender>,
}

impl<Replay> ReplayArchivingWriteReplay<Replay> {
    pub fn new(replay: Replay, archive_sender: Option<ReplayArchiveSender>) -> Self {
        Self {
            replay,
            archive_sender,
        }
    }

    pub fn replay(&self) -> &Replay {
        &self.replay
    }
}

impl<Replay> ReadReplay for ReplayArchivingWriteReplay<Replay>
where
    Replay: ReadReplay,
{
    fn get_context(&self, block_number: BlockNumber) -> Option<BlockContext> {
        self.replay.get_context(block_number)
    }

    fn get_replay_record_by_key(
        &self,
        block_number: BlockNumber,
        db_key: Option<Vec<u8>>,
    ) -> Option<ReplayRecord> {
        self.replay.get_replay_record_by_key(block_number, db_key)
    }

    // SYSCOIN: forward the canonical hash accessor added to local replay storage.
    fn get_canonical_block_hash(&self, block_number: BlockNumber) -> Option<BlockHash> {
        self.replay.get_canonical_block_hash(block_number)
    }

    fn latest_record(&self) -> BlockNumber {
        self.replay.latest_record()
    }
}

impl<Replay> WriteReplay for ReplayArchivingWriteReplay<Replay>
where
    Replay: WriteReplay,
{
    async fn write(
        &self,
        record: Sealed<ReplayRecord>,
        override_allowed: bool,
    ) -> anyhow::Result<bool> {
        let (replay_record, block_hash) = record.clone().split();
        let written = self.replay.write(record, override_allowed).await?;

        let archive_record = if written {
            (block_hash, replay_record)
        } else {
            let block_number = replay_record.block_context.block_number;
            // SYSCOIN: A fresh archive session must backfill WAL records that replay storage
            // already contains after restart. Re-read the canonical value so rejected caller
            // bytes can never be archived in its place.
            let replay_record = self
                .replay
                .get_replay_record(block_number)
                .with_context(|| format!("missing canonical replay record {block_number}"))?;
            let block_hash = self
                .replay
                .get_canonical_block_hash(block_number)
                .with_context(|| format!("missing canonical block hash {block_number}"))?;
            (block_hash, replay_record)
        };

        if let Some(archive_sender) = &self.archive_sender {
            REPLAY_ARCHIVE_METRICS
                .queue_depth
                .set(replay_archive_queue_depth(archive_sender));
            let started_at = Instant::now();
            archive_sender
                .send(archive_record)
                .await
                .context("archive_sender closed")?;
            REPLAY_ARCHIVE_METRICS
                .enqueue_latency
                .observe(started_at.elapsed());
            REPLAY_ARCHIVE_METRICS
                .queue_depth
                .set(replay_archive_queue_depth(archive_sender));
        }

        Ok(written)
    }
}

fn replay_archive_queue_depth(sender: &ReplayArchiveSender) -> usize {
    sender.max_capacity() - sender.capacity()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::B256;

    #[derive(Clone, Debug)]
    struct ExistingReplay {
        record: ReplayRecord,
        block_hash: BlockHash,
    }

    impl ReadReplay for ExistingReplay {
        fn get_context(&self, block_number: BlockNumber) -> Option<BlockContext> {
            (block_number == self.record.block_context.block_number)
                .then_some(self.record.block_context)
        }

        fn get_replay_record_by_key(
            &self,
            block_number: BlockNumber,
            _db_key: Option<Vec<u8>>,
        ) -> Option<ReplayRecord> {
            (block_number == self.record.block_context.block_number).then(|| self.record.clone())
        }

        fn get_canonical_block_hash(&self, block_number: BlockNumber) -> Option<BlockHash> {
            (block_number == self.record.block_context.block_number).then_some(self.block_hash)
        }

        fn latest_record(&self) -> BlockNumber {
            self.record.block_context.block_number
        }
    }

    impl WriteReplay for ExistingReplay {
        async fn write(
            &self,
            _record: Sealed<ReplayRecord>,
            _override_allowed: bool,
        ) -> anyhow::Result<bool> {
            Ok(false)
        }
    }

    #[tokio::test]
    async fn existing_wal_record_backfills_from_canonical_storage() {
        let canonical_record = test_replay_record(7);
        let canonical_hash = B256::with_last_byte(7);
        let replay = ExistingReplay {
            record: canonical_record.clone(),
            block_hash: canonical_hash,
        };
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let archiving = ReplayArchivingWriteReplay::new(replay, Some(sender));

        let mut rejected_record = canonical_record.clone();
        rejected_record.block_output_hash = B256::with_last_byte(9);
        let written = archiving
            .write(
                Sealed::new_unchecked(rejected_record, B256::with_last_byte(9)),
                false,
            )
            .await
            .unwrap();

        assert!(!written);
        assert_eq!(
            receiver.recv().await,
            Some((canonical_hash, canonical_record))
        );
    }

    fn test_replay_record(block_number: BlockNumber) -> ReplayRecord {
        ReplayRecord {
            block_context: BlockContext {
                block_number,
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
