use crate::metrics::BATCH_STORAGE_METRICS;
use alloy::primitives::BlockNumber;
use anyhow::Context;
use std::path::Path;
use zksync_os_batch_types::DiscoveredCommittedBatch;
use zksync_os_rocksdb::RocksDB;
use zksync_os_rocksdb::db::{NamedColumnFamily, WriteBatch as RocksdbWriteBatch};
use zksync_os_storage_api::{PersistedBatch, ReadBatch, WriteBatch};

#[derive(Clone, Debug)]
pub struct ExecutedBatchStorage {
    db: RocksDB<ExecutedBatchColumnFamily>,
}

/// Column families for storage of executed batches.
#[derive(Copy, Clone, Debug)]
pub enum ExecutedBatchColumnFamily {
    /// batch_number (be) => DiscoveredCommittedBatch (JSON)
    BatchInfo,
    /// block_number (be) => batch number which block range starts with this block (be)
    FirstBlockIndex,
    /// Stores the latest appended batch number under a fixed key.
    Latest,
}

impl NamedColumnFamily for ExecutedBatchColumnFamily {
    const DB_NAME: &'static str = "executed_batch_storage";
    const ALL: &'static [Self] = &[
        ExecutedBatchColumnFamily::BatchInfo,
        ExecutedBatchColumnFamily::FirstBlockIndex,
        ExecutedBatchColumnFamily::Latest,
    ];

    fn name(&self) -> &'static str {
        match self {
            ExecutedBatchColumnFamily::BatchInfo => "batch_info",
            ExecutedBatchColumnFamily::FirstBlockIndex => "first_block_index",
            ExecutedBatchColumnFamily::Latest => "latest",
        }
    }
}

impl ExecutedBatchStorage {
    /// Key under `Latest` CF for tracking the highest batch number.
    const LATEST_KEY: &'static [u8] = b"latest_batch";

    pub fn new(db_path: &Path) -> Self {
        let db = RocksDB::<ExecutedBatchColumnFamily>::new(db_path)
            .expect("Failed to open ExecutedBatchStorage");

        Self { db }
    }

    /// Installs or validates the canonical genesis batch.
    ///
    /// SYSCOIN: The stable batch RPC exposes `0` as the empty store's latest batch, so startup
    /// must materialize the real, settlement-verified genesis batch before exposing RPC. This is
    /// separate from [`WriteBatch::write`], whose monotonic append contract always advances the
    /// `Latest` cursor to the written batch number.
    pub fn ensure_genesis(&self, genesis: DiscoveredCommittedBatch) -> anyhow::Result<()> {
        anyhow::ensure!(
            genesis.number() == 0,
            "cannot initialize batch storage with non-genesis batch #{}",
            genesis.number()
        );
        anyhow::ensure!(
            genesis.block_range == (0..=0),
            "canonical genesis batch must contain exactly block 0, got {:?}",
            genesis.block_range
        );

        let expected = PersistedBatch {
            committed_batch: genesis,
            // Genesis is installed by the chain deployment rather than a BlockExecution event.
            execute_sl_block_number: None,
        };
        let genesis_exists = if let Some(stored) = self.get_batch_by_number(0)? {
            anyhow::ensure!(
                stored == expected
                    && stored.batch_info.last_block_timestamp
                        == expected.batch_info.last_block_timestamp,
                "persisted genesis batch does not match the canonical settlement-verified genesis"
            );
            true
        } else {
            false
        };

        let batch_number_key = 0_u64.to_be_bytes();
        let batch_info_value =
            serde_json::to_vec(&expected).context("failed to serialize canonical genesis batch")?;
        let latest = self
            .db
            .get_cf(ExecutedBatchColumnFamily::Latest, Self::LATEST_KEY)
            .context("cannot read latest batch while initializing genesis")?;
        if let Some(latest) = &latest {
            anyhow::ensure!(
                latest.len() == 8,
                "invalid latest batch cursor length: expected 8, got {}",
                latest.len()
            );
            let latest = u64::from_be_bytes(latest.as_slice().try_into().unwrap());
            if !genesis_exists {
                anyhow::ensure!(
                    latest == 0,
                    "executed batch storage is missing canonical genesis but reports latest batch #{latest}; reset the inconsistent batch database before startup"
                );
            }
        }

        // SYSCOIN: Populate the record and block index atomically. A nonzero cursor is accepted only
        // when the exact canonical genesis already exists, which is the normal later-restart case.
        let mut batch: RocksdbWriteBatch<'_, ExecutedBatchColumnFamily> = self.db.new_write_batch();
        batch.put_cf(
            ExecutedBatchColumnFamily::BatchInfo,
            &batch_number_key,
            &batch_info_value,
        );
        batch.put_cf(
            ExecutedBatchColumnFamily::FirstBlockIndex,
            &batch_number_key,
            &batch_number_key,
        );
        if latest.is_none() {
            batch.put_cf(
                ExecutedBatchColumnFamily::Latest,
                Self::LATEST_KEY,
                &batch_number_key,
            );
        }
        BATCH_STORAGE_METRICS
            .data_size
            .observe(batch.size_in_bytes());
        self.db
            .write(batch)
            .context("failed to initialize canonical genesis batch")?;
        BATCH_STORAGE_METRICS
            .persist_batch_number
            .set(self.latest_batch());
        Ok(())
    }

    fn write_batch_unchecked(&self, executed_batch: PersistedBatch) {
        let persist_latency_observer = BATCH_STORAGE_METRICS.persist_latency.start();
        let batch_number_key = executed_batch.number().to_be_bytes().to_vec();
        let first_block_number_key = executed_batch.first_block_number().to_be_bytes().to_vec();
        let batch_info_value = serde_json::to_vec(&executed_batch)
            .expect("failed to serialize DiscoveredCommittedBatch");
        let mut batch: RocksdbWriteBatch<'_, ExecutedBatchColumnFamily> = self.db.new_write_batch();
        batch.put_cf(
            ExecutedBatchColumnFamily::Latest,
            Self::LATEST_KEY,
            &batch_number_key,
        );
        batch.put_cf(
            ExecutedBatchColumnFamily::BatchInfo,
            &batch_number_key,
            &batch_info_value,
        );
        batch.put_cf(
            ExecutedBatchColumnFamily::FirstBlockIndex,
            &first_block_number_key,
            &batch_number_key,
        );
        BATCH_STORAGE_METRICS
            .data_size
            .observe(batch.size_in_bytes());
        self.db
            .write(batch)
            .expect("failed to write to batch storage");
        persist_latency_observer.observe();
        BATCH_STORAGE_METRICS
            .persist_batch_number
            .set(executed_batch.number());
    }
}

impl ReadBatch for ExecutedBatchStorage {
    fn get_batch_by_block_number(
        &self,
        block_number: BlockNumber,
    ) -> anyhow::Result<Option<PersistedBatch>> {
        let block_key = block_number.to_be_bytes();

        let mut iter = self.db.to_iterator_cf(
            ExecutedBatchColumnFamily::FirstBlockIndex,
            ..=block_key.as_slice(),
        );
        if let Some((_, v)) = iter.next() {
            let arr: [u8; 8] = v.as_ref().try_into().context("invalid first block index")?;
            let batch_number = u64::from_be_bytes(arr);
            let batch = self
                .get_batch_by_number(batch_number)?
                .expect("batch indexed in FirstBlockIndex not found in DB");
            if !batch.block_range.contains(&block_number) {
                // This can be hit if requested block number is farther than latest persisted block
                // number.
                return Ok(None);
            }
            Ok(Some(batch))
        } else {
            Ok(None)
        }
    }

    fn get_batch_by_number(&self, batch_number: u64) -> anyhow::Result<Option<PersistedBatch>> {
        let batch_key = batch_number.to_be_bytes();
        let Some(bytes) = self
            .db
            .get_cf(ExecutedBatchColumnFamily::BatchInfo, &batch_key)
            .context("cannot read from DB")?
        else {
            return Ok(None);
        };

        serde_json::from_slice(&bytes).context("failed to deserialize context")
    }

    fn latest_batch(&self) -> u64 {
        self.db
            .get_cf(ExecutedBatchColumnFamily::Latest, Self::LATEST_KEY)
            .expect("cannot read from DB")
            .map(|bytes| {
                assert_eq!(bytes.len(), 8);
                let arr: [u8; 8] = bytes.as_slice().try_into().unwrap();
                u64::from_be_bytes(arr)
            })
            .unwrap_or_default()
    }
}

impl WriteBatch for ExecutedBatchStorage {
    fn write(&self, batch: PersistedBatch) {
        self.write_batch_unchecked(batch)
    }
}

#[cfg(test)]
mod tests {
    use super::ExecutedBatchStorage;
    use alloy::primitives::B256;
    use zksync_os_batch_types::DiscoveredCommittedBatch;
    use zksync_os_contract_interface::models::StoredBatchInfo;
    use zksync_os_storage_api::{PersistedBatch, ReadBatch, WriteBatch};

    fn discovered_batch(
        batch_number: u64,
        first_block: u64,
        last_block: u64,
    ) -> DiscoveredCommittedBatch {
        DiscoveredCommittedBatch {
            batch_info: StoredBatchInfo {
                batch_number,
                state_commitment: B256::with_last_byte(1),
                number_of_layer1_txs: 0,
                priority_operations_hash: B256::with_last_byte(2),
                dependency_roots_rolling_hash: B256::with_last_byte(3),
                l2_to_l1_logs_root_hash: B256::with_last_byte(4),
                commitment: B256::with_last_byte(5),
                last_block_timestamp: Some(0),
            },
            block_range: first_block..=last_block,
        }
    }

    #[test]
    fn initializes_and_reopens_canonical_genesis_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let genesis = discovered_batch(0, 0, 0);
        {
            let storage = ExecutedBatchStorage::new(dir.path());
            storage.ensure_genesis(genesis.clone()).unwrap();
            storage.ensure_genesis(genesis.clone()).unwrap();

            assert_eq!(storage.latest_batch(), 0);
            assert_eq!(
                storage.get_batch_by_number(0).unwrap(),
                Some(PersistedBatch {
                    committed_batch: genesis.clone(),
                    execute_sl_block_number: None,
                })
            );
            assert_eq!(
                storage.get_batch_by_block_number(0).unwrap(),
                storage.get_batch_by_number(0).unwrap()
            );
        }

        let reopened = ExecutedBatchStorage::new(dir.path());
        reopened.ensure_genesis(genesis.clone()).unwrap();
        assert_eq!(
            reopened
                .get_batch_by_number(0)
                .unwrap()
                .unwrap()
                .committed_batch,
            genesis
        );
    }

    #[test]
    fn rejects_conflicting_genesis_without_overwriting_it() {
        let dir = tempfile::tempdir().unwrap();
        let storage = ExecutedBatchStorage::new(dir.path());
        let canonical = discovered_batch(0, 0, 0);
        storage.ensure_genesis(canonical.clone()).unwrap();

        let mut conflicting = canonical.clone();
        conflicting.batch_info.state_commitment = B256::with_last_byte(0xff);
        let err = storage.ensure_genesis(conflicting).unwrap_err();
        assert!(err.to_string().contains("does not match"));
        assert_eq!(
            storage
                .get_batch_by_number(0)
                .unwrap()
                .unwrap()
                .committed_batch,
            canonical
        );

        let mut conflicting_timestamp = canonical.clone();
        conflicting_timestamp.batch_info.last_block_timestamp = None;
        let err = storage.ensure_genesis(conflicting_timestamp).unwrap_err();
        assert!(err.to_string().contains("does not match"));
    }

    #[test]
    fn rejects_a_stale_higher_tip_without_mutating_it() {
        let dir = tempfile::tempdir().unwrap();
        let storage = ExecutedBatchStorage::new(dir.path());
        storage.write(PersistedBatch {
            committed_batch: discovered_batch(7, 70, 79),
            execute_sl_block_number: Some(100),
        });

        let genesis = discovered_batch(0, 0, 0);
        let err = storage.ensure_genesis(genesis).unwrap_err();

        assert!(err.to_string().contains("missing canonical genesis"));
        assert_eq!(storage.latest_batch(), 7);
        assert!(storage.get_batch_by_number(0).unwrap().is_none());
        assert_eq!(storage.get_batch_by_number(7).unwrap().unwrap().number(), 7);
    }

    #[test]
    fn matching_genesis_allows_a_normal_higher_tip_restart() {
        let dir = tempfile::tempdir().unwrap();
        let storage = ExecutedBatchStorage::new(dir.path());
        let genesis = discovered_batch(0, 0, 0);
        storage.ensure_genesis(genesis.clone()).unwrap();
        storage.write(PersistedBatch {
            committed_batch: discovered_batch(7, 70, 79),
            execute_sl_block_number: Some(100),
        });

        storage.ensure_genesis(genesis.clone()).unwrap();

        assert_eq!(storage.latest_batch(), 7);
        assert_eq!(
            storage
                .get_batch_by_number(0)
                .unwrap()
                .unwrap()
                .committed_batch,
            genesis
        );
        assert_eq!(storage.get_batch_by_number(7).unwrap().unwrap().number(), 7);
    }
}
