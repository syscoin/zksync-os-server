use alloy::consensus::{Header, Sealed};
use alloy::primitives::{Address, B256};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::sync::mpsc;
use zksync_os_batch_types::BlockMerkleTreeData;
use zksync_os_interface::error::InvalidTransaction;
use zksync_os_interface::types::{
    AccountDiff, BlockOutput, ExecutionOutput, ExecutionResult, L2ToL1Log, L2ToL1LogWithPreimage,
    StorageWrite, TxOutput,
};
use zksync_os_merkle_tree::{MerkleTree, MerkleTreeVersion, RocksDBWrapper};
use zksync_os_pipeline::{PeekableReceiver, PipelineComponent};
use zksync_os_storage_api::ReplayRecord;

#[derive(Clone, Debug)]
pub struct BatchWorkStorage {
    base_dir: PathBuf,
    run_nonce: Arc<str>,
    next_work_id: Arc<AtomicU64>,
}
// SYSCOIN
#[derive(Clone, Debug)]
pub struct BatchWorkHandle {
    block_number: u64,
    work_id: u64,
}

impl BatchWorkStorage {
    pub fn new(base_dir: impl AsRef<Path>) -> anyhow::Result<Self> {
        let base_dir = base_dir.as_ref().to_owned();
        std::fs::create_dir_all(&base_dir)?;
        clear_stale_batch_work_files(&base_dir)?;
        let run_nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_nanos()
            .to_string();
        Ok(Self {
            base_dir,
            run_nonce: Arc::<str>::from(run_nonce),
            next_work_id: Arc::new(AtomicU64::new(0)),
        })
    }

    fn path_for(&self, handle: &BatchWorkHandle) -> PathBuf {
        self.base_dir.join(format!(
            "block_{}_{}_{}.json",
            handle.block_number, self.run_nonce, handle.work_id
        ))
    }

    fn tmp_path_for(&self, handle: &BatchWorkHandle) -> PathBuf {
        self.base_dir.join(format!(
            "block_{}_{}_{}.json.tmp",
            handle.block_number, self.run_nonce, handle.work_id
        ))
    }

    pub async fn store(
        &self,
        block_output: BlockOutput,
        replay_record: ReplayRecord,
    ) -> anyhow::Result<BatchWorkHandle> {
        let handle = BatchWorkHandle {
            block_number: block_output.header.number,
            work_id: self.next_work_id.fetch_add(1, Ordering::Relaxed),
        };
        let data = serde_json::to_vec(&BatchWorkItem::from_parts(block_output, replay_record))?;
        let tmp_path = self.tmp_path_for(&handle);
        let path = self.path_for(&handle);
        fs::write(&tmp_path, data).await?;
        fs::rename(&tmp_path, &path).await?;
        Ok(handle)
    }

    pub async fn load(&self, handle: &BatchWorkHandle) -> anyhow::Result<BatchWorkItem> {
        let data = fs::read(self.path_for(handle)).await?;
        Ok(serde_json::from_slice(&data)?)
    }

    pub async fn delete(&self, handle: &BatchWorkHandle) -> anyhow::Result<()> {
        let path = self.path_for(handle);
        if fs::try_exists(&path).await? {
            fs::remove_file(path).await?;
        }
        Ok(())
    }
}
// SYSCOIN
fn clear_stale_batch_work_files(base_dir: &Path) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(base_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if name.starts_with("block_") && (name.ends_with(".json") || name.ends_with(".json.tmp")) {
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}

pub struct BatchWorkDispatcher {
    storage: BatchWorkStorage,
    sender: mpsc::Sender<BatchWorkHandle>,
}

impl BatchWorkDispatcher {
    pub fn new(storage: BatchWorkStorage, sender: mpsc::Sender<BatchWorkHandle>) -> Self {
        Self { storage, sender }
    }
}

#[async_trait]
impl PipelineComponent for BatchWorkDispatcher {
    type Input = (BlockOutput, ReplayRecord, BlockMerkleTreeData);
    type Output = ();

    const NAME: &'static str = "batch_work_dispatcher";
    const OUTPUT_BUFFER_SIZE: usize = 1;

    async fn run(
        self,
        mut input: PeekableReceiver<Self::Input>,
        _output: mpsc::Sender<Self::Output>,
    ) -> anyhow::Result<()> {
        while let Some((block_output, replay_record, _tree)) = input.recv().await {
            let block_number = replay_record.block_context.block_number;
            let handle = self.storage.store(block_output, replay_record).await?;
            anyhow::ensure!(
                handle.block_number == block_number,
                "batch work block number mismatch: replay {block_number}, output {}",
                handle.block_number
            );
            self.sender
                .send(handle)
                .await
                .map_err(|_| anyhow::anyhow!("batch work receiver dropped"))?;
        }
        tracing::info!("inbound channel closed");
        Ok(())
    }
}

pub struct BatchWorkSource {
    storage: BatchWorkStorage,
    tree: MerkleTree<RocksDBWrapper>,
    receiver: mpsc::Receiver<BatchWorkHandle>,
}

impl BatchWorkSource {
    pub fn new(
        storage: BatchWorkStorage,
        tree: MerkleTree<RocksDBWrapper>,
        receiver: mpsc::Receiver<BatchWorkHandle>,
    ) -> Self {
        Self {
            storage,
            tree,
            receiver,
        }
    }
}

#[async_trait]
impl PipelineComponent for BatchWorkSource {
    type Input = ();
    type Output = (BlockOutput, ReplayRecord, BlockMerkleTreeData);

    const NAME: &'static str = "batch_work_source";
    const OUTPUT_BUFFER_SIZE: usize = 5;

    async fn run(
        mut self,
        _input: PeekableReceiver<Self::Input>,
        output: mpsc::Sender<Self::Output>,
    ) -> anyhow::Result<()> {
        while let Some(handle) = self.receiver.recv().await {
            let block_number = handle.block_number;
            // SYSCOIN
            let item = self.storage.load(&handle).await?;
            let (block_output, replay_record) = item.into_parts();
            let tree = BlockMerkleTreeData {
                block_start: MerkleTreeVersion {
                    tree: self.tree.clone(),
                    block: block_number - 1,
                },
                block_end: MerkleTreeVersion {
                    tree: self.tree.clone(),
                    block: block_number,
                },
            };
            if output
                .send((block_output, replay_record, tree))
                .await
                .is_err()
            {
                tracing::info!("outbound channel closed");
                return Ok(());
            }
            self.storage.delete(&handle).await?;
        }
        tracing::info!("batch work channel closed");
        Ok(())
    }
}
// SYSCOIN
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BatchWorkItem {
    block_output: BatchWorkBlockOutput,
    replay_record: ReplayRecord,
}

impl BatchWorkItem {
    fn from_parts(block_output: BlockOutput, replay_record: ReplayRecord) -> Self {
        Self {
            block_output: BatchWorkBlockOutput::from(block_output),
            replay_record,
        }
    }

    fn into_parts(self) -> (BlockOutput, ReplayRecord) {
        (self.block_output.into_block_output(), self.replay_record)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BatchWorkBlockOutput {
    header: BatchWorkHeader,
    tx_results: Vec<Option<BatchWorkTxOutput>>,
    pubdata: Vec<u8>,
    computational_native_used: u64,
}

impl From<BlockOutput> for BatchWorkBlockOutput {
    fn from(block_output: BlockOutput) -> Self {
        Self {
            header: BatchWorkHeader {
                number: block_output.header.number,
                timestamp: block_output.header.timestamp,
                hash: block_output.header.hash(),
            },
            tx_results: block_output
                .tx_results
                .into_iter()
                .map(|result| result.ok().map(BatchWorkTxOutput::from))
                .collect(),
            pubdata: block_output.pubdata,
            computational_native_used: block_output.computational_native_used,
        }
    }
}

impl BatchWorkBlockOutput {
    fn into_block_output(self) -> BlockOutput {
        let mut header = Header::default();
        header.number = self.header.number;
        header.timestamp = self.header.timestamp;

        BlockOutput {
            header: Sealed::new_unchecked(header, self.header.hash),
            tx_results: self
                .tx_results
                .into_iter()
                .map(|result| match result {
                    Some(output) => Ok(output.into_tx_output()),
                    None => Err(InvalidTransaction::InvalidStructure),
                })
                .collect(),
            storage_writes: Vec::<StorageWrite>::new(),
            account_diffs: Vec::<AccountDiff>::new(),
            published_preimages: Vec::new(),
            pubdata: self.pubdata,
            computational_native_used: self.computational_native_used,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BatchWorkHeader {
    number: u64,
    timestamp: u64,
    hash: B256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BatchWorkTxOutput {
    l2_to_l1_logs: Vec<BatchWorkL2ToL1LogWithPreimage>,
}

impl From<TxOutput> for BatchWorkTxOutput {
    fn from(output: TxOutput) -> Self {
        Self {
            l2_to_l1_logs: output
                .l2_to_l1_logs
                .into_iter()
                .map(BatchWorkL2ToL1LogWithPreimage::from)
                .collect(),
        }
    }
}

impl BatchWorkTxOutput {
    fn into_tx_output(self) -> TxOutput {
        TxOutput {
            execution_result: ExecutionResult::Success(ExecutionOutput::Call(Vec::new())),
            gas_used: 0,
            gas_refunded: 0,
            computational_native_used: 0,
            native_used: 0,
            pubdata_used: 0,
            contract_address: None,
            logs: Vec::new(),
            l2_to_l1_logs: self
                .l2_to_l1_logs
                .into_iter()
                .map(BatchWorkL2ToL1LogWithPreimage::into_l2_to_l1_log_with_preimage)
                .collect(),
            storage_writes: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BatchWorkL2ToL1LogWithPreimage {
    log: BatchWorkL2ToL1Log,
    preimage: Option<Vec<u8>>,
}

impl From<L2ToL1LogWithPreimage> for BatchWorkL2ToL1LogWithPreimage {
    fn from(log: L2ToL1LogWithPreimage) -> Self {
        Self {
            log: BatchWorkL2ToL1Log::from(log.log),
            preimage: log.preimage,
        }
    }
}

impl BatchWorkL2ToL1LogWithPreimage {
    fn into_l2_to_l1_log_with_preimage(self) -> L2ToL1LogWithPreimage {
        L2ToL1LogWithPreimage {
            log: self.log.into_l2_to_l1_log(),
            preimage: self.preimage,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BatchWorkL2ToL1Log {
    l2_shard_id: u8,
    is_service: bool,
    tx_number_in_block: u16,
    sender: Address,
    key: B256,
    value: B256,
}

impl From<L2ToL1Log> for BatchWorkL2ToL1Log {
    fn from(log: L2ToL1Log) -> Self {
        Self {
            l2_shard_id: log.l2_shard_id,
            is_service: log.is_service,
            tx_number_in_block: log.tx_number_in_block,
            sender: log.sender,
            key: log.key,
            value: log.value,
        }
    }
}

impl BatchWorkL2ToL1Log {
    fn into_l2_to_l1_log(self) -> L2ToL1Log {
        L2ToL1Log {
            l2_shard_id: self.l2_shard_id,
            is_service: self.is_service,
            tx_number_in_block: self.tx_number_in_block,
            sender: self.sender,
            key: self.key,
            value: self.value,
        }
    }
}
