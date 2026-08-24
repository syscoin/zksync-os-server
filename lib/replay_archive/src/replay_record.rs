use crate::metrics::REPLAY_ARCHIVE_METRICS;
use crate::{ReplayArchiveStorage, ReplayArchiver};
use alloy::primitives::{BlockHash, BlockNumber};
use async_trait::async_trait;
use zksync_os_storage_api::ReplayRecord;

/// Replay archiver that stores JSON-encoded replay records without encryption.
#[derive(Debug, Clone)]
pub struct ReplayRecordArchiver<Storage> {
    storage: Storage,
}

impl<Storage> ReplayRecordArchiver<Storage> {
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }

    pub fn storage(&self) -> &Storage {
        &self.storage
    }
}

#[async_trait]
impl<Storage> ReplayArchiver for ReplayRecordArchiver<Storage>
where
    Storage: ReplayArchiveStorage,
{
    async fn append_replay_record(
        &self,
        block_hash: BlockHash,
        replay_record: ReplayRecord,
    ) -> anyhow::Result<()> {
        let block_number = replay_record.block_context.block_number;
        // SYSCOIN: a session append must encode and publish this writer's record; shared-key
        // presence is not proof that the expected payload was archived.
        let encoded = encode_replay_record(&replay_record);
        REPLAY_ARCHIVE_METRICS.object_bytes[&"stored"].observe(encoded.len());
        self.storage
            .append_object(block_number, block_hash, encoded)
            .await
    }

    async fn contains_replay_record(
        &self,
        block_number: BlockNumber,
        block_hash: BlockHash,
    ) -> anyhow::Result<bool> {
        self.storage.contains_object(block_number, block_hash).await
    }
}

pub(crate) fn encode_replay_record(replay_record: &ReplayRecord) -> Vec<u8> {
    serde_json::to_vec(replay_record).expect("failed to encode replay record")
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::B256;
    use serde_json::Value;
    use zksync_os_storage_api::BlockContext;

    fn replay_record() -> ReplayRecord {
        ReplayRecord {
            block_context: BlockContext {
                block_number: 7,
                ..Default::default()
            },
            transactions: Vec::new(),
            previous_block_timestamp: 11,
            node_version: "0.22.0".parse().unwrap(),
            protocol_version: "0.32.0".parse().unwrap(),
            block_output_hash: B256::repeat_byte(0x11),
            force_preimages: Vec::new(),
            starting_cursors: Default::default(),
        }
    }

    #[test]
    fn retired_upgrade_metadata_is_ignored_and_not_reserialized() {
        let expected = replay_record();
        let mut archived = serde_json::to_value(&expected).unwrap();
        archived.as_object_mut().unwrap().insert(
            "canonical_upgrade_tx_hash".to_owned(),
            Value::String(format!("0x{}", "5a".repeat(32))),
        );

        let decoded: ReplayRecord = serde_json::from_value(archived).unwrap();
        assert_eq!(decoded, expected);

        let reserialized = serde_json::to_value(decoded).unwrap();
        assert!(reserialized.get("canonical_upgrade_tx_hash").is_none());
    }
}
