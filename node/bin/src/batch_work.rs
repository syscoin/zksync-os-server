use alloy::consensus::{Header, Sealed};
use alloy::primitives::{Address, B256};
use anyhow::Context;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::sync::{mpsc, watch};
use zksync_os_batch_types::BlockMerkleTreeData;
use zksync_os_interface::error::InvalidTransaction;
use zksync_os_interface::types::{
    AccountDiff, ExecutionOutput, ExecutionResult, L2ToL1Log, L2ToL1LogWithPreimage, StorageWrite,
    TxOutput,
};
use zksync_os_merkle_tree::{BatchTreeProof, MerkleTree, RocksDBWrapper, TreeBatchOutput};
use zksync_os_observability::ComponentStateReporter;
use zksync_os_pipeline::{PeekableReceiver, PipelineComponent, SendAndRecordExt};
use zksync_os_sequencer::model::blocks::{BlockCommandType, BlockPayload};
use zksync_os_storage_api::{ReplayRecord, TreeBlock};
use zksync_os_types::{BlockOutput, BlockPubdata, SYSCOIN_COMPACT_EDGE_DA_COMMIT_TARGET};

// SYSCOIN: Target discovery may legitimately require several newly executed Gateway blocks, but
// unauthenticated production must remain finitely bounded on disk. Startup replay through the
// durable frontier is exempt so a long canonical rebuild cannot deadlock target discovery.
const MAX_UNAUTHENTICATED_LIVE_BATCH_WORK: usize = 1024;
// SYSCOIN: Give a concurrently publishing authenticated watch value one bounded final chance.
const TARGET_AUTHENTICATION_GRACE_PERIOD: Duration = Duration::from_secs(30);
// SYSCOIN: Preserve the original unauthenticated epoch across supervisor restarts so the live
// allowance cannot reset until the exact app-bound target has authenticated.
const UNAUTHENTICATED_EPOCH_FRONTIER_FILE: &str = "unauthenticated_epoch_frontier";
// SYSCOIN: Full startup replay is allowed to cross the live-block count gate, but its compact
// sidecar must never consume the volume without bound. An operator needing more than 8 GiB must
// restore from a closer canonical snapshot instead of risking RocksDB volume exhaustion.
const MAX_BATCH_WORK_STAGING_BYTES: u64 = 8 * 1024 * 1024 * 1024;

// SYSCOIN: A published watch value is not authority by itself. Both the pre-persistence
// production gate and the post-tree release gate require the exact app-bound address.
async fn wait_for_authenticated_target(
    target_source: &mut watch::Receiver<Option<Address>>,
) -> anyhow::Result<()> {
    let target = target_source
        .wait_for(Option::is_some)
        .await
        .context("compact Edge-DA target source closed before authentication")?
        .expect("watch predicate requires a target");
    anyhow::ensure!(
        target == SYSCOIN_COMPACT_EDGE_DA_COMMIT_TARGET,
        "batch-work target {target} does not match app-bound target {SYSCOIN_COMPACT_EDGE_DA_COMMIT_TARGET}"
    );
    Ok(())
}

fn authenticated_target_is_ready(
    target_source: &mut watch::Receiver<Option<Address>>,
) -> anyhow::Result<bool> {
    let target = *target_source.borrow_and_update();
    if let Some(target) = target {
        anyhow::ensure!(
            target == SYSCOIN_COMPACT_EDGE_DA_COMMIT_TARGET,
            "batch-work target {target} does not match app-bound target {SYSCOIN_COMPACT_EDGE_DA_COMMIT_TARGET}"
        );
        return Ok(true);
    }
    target_source
        .has_changed()
        .context("compact Edge-DA target source closed before authentication")?;
    Ok(false)
}

#[derive(Clone, Debug)]
pub struct BatchWorkStorage {
    base_dir: PathBuf,
    run_nonce: Arc<str>,
    next_work_id: Arc<AtomicU64>,
    staged_bytes: Arc<AtomicU64>,
    max_staged_bytes: u64,
}
// SYSCOIN: Opaque staging handle binds one serialized artifact to its block/work identity and
// exact byte debit; callers cannot substitute a path or bypass the bounded storage ledger.
#[derive(Clone, Debug)]
pub struct BatchWorkHandle {
    block_number: u64,
    work_id: u64,
    byte_len: u64,
}

impl BatchWorkStorage {
    pub fn new(base_dir: impl AsRef<Path>) -> anyhow::Result<Self> {
        Self::new_with_staging_limit(base_dir, MAX_BATCH_WORK_STAGING_BYTES)
    }

    fn new_with_staging_limit(
        base_dir: impl AsRef<Path>,
        max_staged_bytes: u64,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            max_staged_bytes > 0,
            "batch-work staging limit must be positive"
        );
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
            staged_bytes: Arc::new(AtomicU64::new(0)),
            max_staged_bytes,
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

    // SYSCOIN: This tiny durable latch is created before execution starts. A corrupt or
    // snapshot-inconsistent latch fails closed; only exact target authentication removes it.
    pub fn begin_unauthenticated_epoch(&self, current_frontier: u64) -> anyhow::Result<u64> {
        let path = self.base_dir.join(UNAUTHENTICATED_EPOCH_FRONTIER_FILE);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                anyhow::ensure!(
                    metadata.file_type().is_file() && metadata.len() <= 20,
                    "invalid unauthenticated batch-work epoch marker"
                );
                let value = std::fs::read_to_string(&path)?;
                let frontier = value
                    .trim()
                    .parse::<u64>()
                    .context("invalid unauthenticated batch-work epoch frontier")?;
                anyhow::ensure!(
                    frontier <= current_frontier,
                    "unauthenticated batch-work epoch frontier {frontier} is ahead of canonical startup frontier {current_frontier}"
                );
                Ok(frontier)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut options = OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                options.mode(0o600);
                let mut file = options.open(&path)?;
                file.write_all(current_frontier.to_string().as_bytes())?;
                file.sync_all()?;
                std::fs::File::open(&self.base_dir)?.sync_all()?;
                Ok(current_frontier)
            }
            Err(error) => Err(error.into()),
        }
    }

    // SYSCOIN: Removal is itself durable so an authenticated epoch cannot reappear after a crash.
    fn clear_unauthenticated_epoch(&self) -> anyhow::Result<()> {
        let path = self.base_dir.join(UNAUTHENTICATED_EPOCH_FRONTIER_FILE);
        match std::fs::remove_file(path) {
            Ok(()) => std::fs::File::open(&self.base_dir)?
                .sync_all()
                .map_err(Into::into),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn store(
        &self,
        block_output: BlockOutput,
        replay_record: ReplayRecord,
        tree: Option<BlockMerkleTreeData>,
    ) -> anyhow::Result<BatchWorkHandle> {
        let block_number = block_output.header.number;
        let data = serde_json::to_vec(&BatchWorkItem::from_parts(
            block_output,
            replay_record,
            tree,
        ))?;
        let byte_len =
            u64::try_from(data.len()).context("batch-work item length does not fit u64")?;
        self.staged_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(byte_len)
                    .filter(|next| *next <= self.max_staged_bytes)
            })
            .map_err(|current| {
                anyhow::anyhow!(
                    "batch-work staging limit {} bytes exceeded by {byte_len}-byte item at {current} bytes",
                    self.max_staged_bytes
                )
            })?;
        let handle = BatchWorkHandle {
            block_number,
            work_id: self.next_work_id.fetch_add(1, Ordering::Relaxed),
            byte_len,
        };
        let tmp_path = self.tmp_path_for(&handle);
        let path = self.path_for(&handle);
        let write_result: std::io::Result<()> = async {
            fs::write(&tmp_path, data).await?;
            fs::rename(&tmp_path, &path).await?;
            Ok(())
        }
        .await;
        if let Err(error) = write_result {
            // SYSCOIN: Best-effort cleanup keeps the per-run byte ledger aligned even before the
            // critical-component failure shuts the process down; startup also clears any remnant.
            let _ = fs::remove_file(&tmp_path).await;
            self.staged_bytes.fetch_sub(byte_len, Ordering::AcqRel);
            return Err(error.into());
        }
        Ok(handle)
    }

    async fn load(&self, handle: &BatchWorkHandle) -> anyhow::Result<BatchWorkItem> {
        let data = fs::read(self.path_for(handle)).await?;
        Ok(serde_json::from_slice(&data)?)
    }

    pub async fn delete(&self, handle: &BatchWorkHandle) -> anyhow::Result<()> {
        let path = self.path_for(handle);
        if fs::try_exists(&path).await? {
            fs::remove_file(path).await?;
            let previous = self
                .staged_bytes
                .fetch_sub(handle.byte_len, Ordering::AcqRel);
            debug_assert!(previous >= handle.byte_len);
        }
        Ok(())
    }
}
// SYSCOIN: Batch-work payloads are reconstructable from canonical DB state. Remove only stale
// per-block staging files at process start while preserving the durable security-epoch marker.
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

// SYSCOIN: Enforce the unresolved-target production ceiling before a payload can be proposed to
// consensus or persisted. The downstream dispatcher remains responsible only for durable staging
// and ordered release once this gate authenticates the exact app-bound target.
pub struct BatchWorkPersistenceGate {
    storage: BatchWorkStorage,
    authenticated_target: watch::Receiver<Option<Address>>,
    startup_replay_frontier: u64,
    unauthenticated_live_batch_work: usize,
    max_unauthenticated_live_batch_work: usize,
    target_authentication_grace_period: Duration,
    target_ready: bool,
}

impl BatchWorkPersistenceGate {
    pub fn new(
        storage: BatchWorkStorage,
        authenticated_target: watch::Receiver<Option<Address>>,
        startup_replay_frontier: u64,
    ) -> anyhow::Result<Self> {
        let unauthenticated_epoch_frontier =
            storage.begin_unauthenticated_epoch(startup_replay_frontier)?;
        Ok(Self::new_with_unauthenticated_live_limit(
            storage,
            authenticated_target,
            startup_replay_frontier,
            unauthenticated_epoch_frontier,
            MAX_UNAUTHENTICATED_LIVE_BATCH_WORK,
            TARGET_AUTHENTICATION_GRACE_PERIOD,
        ))
    }

    fn new_with_unauthenticated_live_limit(
        storage: BatchWorkStorage,
        authenticated_target: watch::Receiver<Option<Address>>,
        startup_replay_frontier: u64,
        unauthenticated_epoch_frontier: u64,
        max_unauthenticated_live_batch_work: usize,
        target_authentication_grace_period: Duration,
    ) -> Self {
        assert!(max_unauthenticated_live_batch_work > 0);
        assert!(unauthenticated_epoch_frontier <= startup_replay_frontier);
        let unauthenticated_live_batch_work =
            usize::try_from(startup_replay_frontier - unauthenticated_epoch_frontier)
                .unwrap_or(usize::MAX)
                .min(max_unauthenticated_live_batch_work);
        Self {
            storage,
            authenticated_target,
            startup_replay_frontier,
            unauthenticated_live_batch_work,
            max_unauthenticated_live_batch_work,
            target_authentication_grace_period,
            target_ready: false,
        }
    }

    fn observe_authenticated_target(&mut self) -> anyhow::Result<bool> {
        if self.target_ready {
            return Ok(true);
        }
        if authenticated_target_is_ready(&mut self.authenticated_target)? {
            self.storage.clear_unauthenticated_epoch()?;
            self.target_ready = true;
        }
        Ok(self.target_ready)
    }

    async fn admit_block(
        &mut self,
        command_type: BlockCommandType,
        block_number: u64,
    ) -> anyhow::Result<()> {
        let target_ready = self.observe_authenticated_target()?;
        if block_number <= self.startup_replay_frontier {
            return Ok(());
        }
        // SYSCOIN: Rebuild commands are startup-only and must refer to the durable startup
        // frontier handled above. Reject the impossible post-frontier form even after target
        // authentication so a future caller cannot silently turn it into a production command.
        if matches!(
            command_type,
            BlockCommandType::Rebuild | BlockCommandType::CanonizedRebuild
        ) {
            anyhow::bail!(
                "post-frontier rebuild block {block_number} cannot bypass compact Edge-DA target authentication"
            );
        }
        if target_ready {
            return Ok(());
        }
        match command_type {
            // SYSCOIN: A Replay is already canonical in Raft. It must reach persistence because it
            // may itself install the authenticated target. Count it toward this node's cumulative
            // epoch ceiling so a later leadership transition cannot obtain a fresh allowance.
            BlockCommandType::Replay => {
                self.unauthenticated_live_batch_work = self
                    .unauthenticated_live_batch_work
                    .saturating_add(1)
                    .min(self.max_unauthenticated_live_batch_work);
                return Ok(());
            }
            BlockCommandType::Rebuild | BlockCommandType::CanonizedRebuild => {
                unreachable!("post-frontier rebuild rejected above")
            }
            BlockCommandType::Produce => {}
        }
        if self.unauthenticated_live_batch_work < self.max_unauthenticated_live_batch_work {
            self.unauthenticated_live_batch_work += 1;
            return Ok(());
        }

        let grace_period = self.target_authentication_grace_period;
        tokio::time::timeout(
            grace_period,
            wait_for_authenticated_target(&mut self.authenticated_target),
        )
        .await
        .with_context(|| {
            format!(
                "compact Edge-DA target authentication timed out after {grace_period:?} at the unauthenticated live batch-work limit {}",
                self.max_unauthenticated_live_batch_work
            )
        })??;
        self.storage.clear_unauthenticated_epoch()?;
        self.target_ready = true;
        Ok(())
    }
}

#[async_trait]
impl PipelineComponent for BatchWorkPersistenceGate {
    type Input = BlockPayload;
    type Output = BlockPayload;

    const COMPONENT_ID: zksync_os_pipeline::ComponentId =
        zksync_os_pipeline::ComponentId::BatchWorkPersistenceGate;
    const OUTPUT_CHANNEL_CAPACITY: usize = 1;

    async fn run(
        mut self,
        mut input: PeekableReceiver<Self::Input>,
        output: mpsc::Sender<Self::Output>,
        state_reporter: ComponentStateReporter,
    ) -> anyhow::Result<()> {
        while let Some(payload) = input.recv_and_record_picked(&state_reporter).await {
            self.admit_block(
                payload.command_type,
                payload.record.block_context.block_number,
            )
            .await?;
            output.send_and_record(payload, &state_reporter).await?;
        }
        tracing::info!("inbound channel closed");
        Ok(())
    }
}

pub struct BatchWorkDispatcher {
    storage: BatchWorkStorage,
    sender: mpsc::Sender<BatchWorkHandle>,
    // SYSCOIN: `Some` only when compact Edge-DA authentication is required. While it is None-valued
    // the dispatcher persists execution output but must not release work to the publishing batcher.
    authenticated_target: Option<watch::Receiver<Option<Address>>>,
}

impl BatchWorkDispatcher {
    pub fn new(
        storage: BatchWorkStorage,
        sender: mpsc::Sender<BatchWorkHandle>,
        authenticated_target: Option<watch::Receiver<Option<Address>>>,
    ) -> Self {
        Self {
            storage,
            sender,
            authenticated_target,
        }
    }

    // SYSCOIN: A watch publication is not itself authority. Every delivery boundary rechecks the
    // exact app-bound target, and a closed unresolved source fails startup rather than draining.
    async fn wait_for_authenticated_target(&mut self) -> anyhow::Result<()> {
        let Some(target_source) = self.authenticated_target.as_mut() else {
            return Ok(());
        };
        wait_for_authenticated_target(target_source).await
    }

    async fn send_handle(&self, handle: BatchWorkHandle) -> anyhow::Result<()> {
        self.sender
            .send(handle)
            .await
            .map_err(|_| anyhow::anyhow!("batch work receiver dropped"))
    }
}

#[async_trait]
impl PipelineComponent for BatchWorkDispatcher {
    type Input = TreeBlock;
    type Output = ();

    const COMPONENT_ID: zksync_os_pipeline::ComponentId =
        zksync_os_pipeline::ComponentId::BatchWorkDispatcher;
    const OUTPUT_CHANNEL_CAPACITY: usize = 1;

    async fn run(
        mut self,
        mut input: PeekableReceiver<Self::Input>,
        _output: mpsc::Sender<Self::Output>,
        _state_reporter: ComponentStateReporter,
    ) -> anyhow::Result<()> {
        let mut target_ready = self.authenticated_target.is_none();
        let mut staged_handles = VecDeque::new();
        // SYSCOIN: The pre-persistence gate owns the finite production ceiling. This downstream
        // component may therefore stage every admitted block compactly while authentication is
        // pending, without duplicating security accounting after canonization.
        while !target_ready {
            tokio::select! {
                biased;
                result = self.wait_for_authenticated_target() => {
                    result?;
                    target_ready = true;
                }
                block = input.recv() => {
                    let Some(TreeBlock {
                        output: block_output,
                        record: replay_record,
                        tree: _,
                    }) = block else {
                        anyhow::bail!("batch work input closed before compact Edge-DA target authentication");
                    };
                    let block_number = replay_record.block_context.block_number;
                    // SYSCOIN: During the exceptional pre-auth window, persist compact identity
                    // only; canonical tree versions provide the deferred read-only witness.
                    let handle = self.storage.store(block_output, replay_record, None).await?;
                    anyhow::ensure!(
                        handle.block_number == block_number,
                        "batch work block number mismatch: replay {block_number}, output {}",
                        handle.block_number
                    );
                    staged_handles.push_back(handle);
                }
            }
        }

        if self.authenticated_target.is_some() {
            self.storage.clear_unauthenticated_epoch()?;
        }

        // SYSCOIN: Flush only the lightweight in-process index after authentication. Full work is
        // already durable; a crash discards this index and canonical replay reconstructs it.
        while let Some(handle) = staged_handles.pop_front() {
            self.send_handle(handle).await?;
        }

        while let Some(TreeBlock {
            output: block_output,
            record: replay_record,
            tree,
        }) = input.recv().await
        {
            let block_number = replay_record.block_context.block_number;
            // SYSCOIN: Authenticated production retains the efficient update proof; only deferred
            // bootstrap work pays the canonical tree-fallback I/O cost.
            let handle = self
                .storage
                .store(block_output, replay_record, Some(tree))
                .await?;
            anyhow::ensure!(
                handle.block_number == block_number,
                "batch work block number mismatch: replay {block_number}, output {}",
                handle.block_number
            );
            self.send_handle(handle).await?;
        }
        tracing::info!("inbound channel closed");
        Ok(())
    }
}

pub struct BatchWorkSource {
    storage: BatchWorkStorage,
    receiver: mpsc::Receiver<BatchWorkHandle>,
    tree: MerkleTree<RocksDBWrapper>,
}

impl BatchWorkSource {
    pub fn new(
        storage: BatchWorkStorage,
        receiver: mpsc::Receiver<BatchWorkHandle>,
        tree: MerkleTree<RocksDBWrapper>,
    ) -> Self {
        Self {
            storage,
            receiver,
            tree,
        }
    }

    // SYSCOIN: The canonical tree versions are already durable before TreeManager emits a block.
    // Deferred work needs no serialized update proof: native PIG falls back read-only to version
    // `block_number - 1` for every query omitted from this deliberately empty cache.
    fn empty_tree_data(&self, block_number: u64) -> anyhow::Result<BlockMerkleTreeData> {
        let previous_block_number = block_number
            .checked_sub(1)
            .context("batch work cannot reconstruct a tree witness for genesis")?;
        let (input_root, input_leaf_count) = self
            .tree
            .root_info(previous_block_number)?
            .with_context(|| {
                format!("Merkle tree is missing input version {previous_block_number}")
            })?;
        let (output_root, output_leaf_count) = self
            .tree
            .root_info(block_number)?
            .with_context(|| format!("Merkle tree is missing output version {block_number}"))?;
        Ok(BlockMerkleTreeData {
            input: TreeBatchOutput {
                root_hash: input_root,
                leaf_count: input_leaf_count,
            },
            output: TreeBatchOutput {
                root_hash: output_root,
                leaf_count: output_leaf_count,
            },
            written_keys: Vec::new(),
            read_keys: Vec::new(),
            proof: BatchTreeProof {
                operations: Vec::new(),
                read_operations: Vec::new(),
                sorted_leaves: BTreeMap::new(),
                hashes: Vec::new(),
            },
        })
    }
}

#[async_trait]
impl PipelineComponent for BatchWorkSource {
    type Input = ();
    type Output = TreeBlock;

    const COMPONENT_ID: zksync_os_pipeline::ComponentId =
        zksync_os_pipeline::ComponentId::BatchWorkSource;
    const OUTPUT_CHANNEL_CAPACITY: usize = 5;

    async fn run(
        mut self,
        _input: PeekableReceiver<Self::Input>,
        output: mpsc::Sender<Self::Output>,
        state_reporter: ComponentStateReporter,
    ) -> anyhow::Result<()> {
        while let Some(handle) = self.receiver.recv().await {
            // SYSCOIN: Resolve the opaque handle inside the storage boundary before ordered
            // release, keeping the staging path and byte-accounting ledger internal.
            let item = self.storage.load(&handle).await?;
            let (block_output, replay_record, tree) = item.into_parts();
            let block_number = replay_record.block_context.block_number;
            let block_timestamp = replay_record.block_context.timestamp;
            let tree = match tree {
                Some(tree) => tree,
                None => self.empty_tree_data(block_number)?,
            };
            let tree_block = TreeBlock {
                output: block_output,
                record: replay_record,
                tree,
            };
            // SYSCOIN: persisted catch-up work can be longer than any fixed channel
            // capacity. Await downstream capacity here instead of treating normal
            // recovery backpressure as catastrophic pipeline failure.
            output
                .send(tree_block)
                .await
                .map_err(|_| anyhow::anyhow!("batch work consumer dropped"))?;
            state_reporter.record_processed(block_number, Some(block_timestamp), None);
            self.storage.delete(&handle).await?;
        }
        tracing::info!("batch work channel closed");
        Ok(())
    }
}
// SYSCOIN: Serialized staging envelope preserves the canonical block/replay pair and optional
// upstream tree proof across the asynchronous target-authentication gate.
#[derive(Debug, Serialize, Deserialize)]
struct BatchWorkItem {
    block_output: BatchWorkBlockOutput,
    replay_record: ReplayRecord,
    // SYSCOIN: Absent only for target-gated bootstrap work; authenticated production persists the
    // upstream batched proof so normal prover-input generation retains its zero-I/O fast path.
    tree: Option<BlockMerkleTreeData>,
}

impl BatchWorkItem {
    fn from_parts(
        block_output: BlockOutput,
        replay_record: ReplayRecord,
        tree: Option<BlockMerkleTreeData>,
    ) -> Self {
        Self {
            block_output: BatchWorkBlockOutput::from(block_output),
            replay_record,
            tree,
        }
    }

    fn into_parts(self) -> (BlockOutput, ReplayRecord, Option<BlockMerkleTreeData>) {
        (
            self.block_output.into_block_output(),
            self.replay_record,
            self.tree,
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BatchWorkBlockOutput {
    header: BatchWorkHeader,
    tx_results: Vec<Option<BatchWorkTxOutput>>,
    pubdata_used: u64,
    computational_native_used: u64,
}

impl From<BlockOutput> for BatchWorkBlockOutput {
    fn from(block_output: BlockOutput) -> Self {
        let pubdata_used = block_output.pubdata_used();
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
            pubdata_used,
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
            pubdata: BlockPubdata::new(self.pubdata_used),
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
    // SYSCOIN: Reverted VM outputs may retain diagnostic L2-to-L1 logs. Preserve status so batch
    // replay cannot mistake those logs for durable interop or compact Edge-DA messages.
    success: bool,
    l2_to_l1_logs: Vec<BatchWorkL2ToL1LogWithPreimage>,
}

impl From<TxOutput> for BatchWorkTxOutput {
    fn from(output: TxOutput) -> Self {
        Self {
            success: output.is_success(),
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
            execution_result: if self.success {
                ExecutionResult::Success(ExecutionOutput::Call(Vec::new()))
            } else {
                ExecutionResult::Revert(Vec::new())
            },
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::consensus::Sealable;
    use std::collections::BTreeMap;
    use tokio::sync::mpsc::error::TryRecvError;
    use zksync_os_merkle_tree::{BatchTreeProof, TreeBatchOutput, TreeEntry};
    use zksync_os_native_pig::tree::{EfficientTreeAdapter, RawLeafProof, VersionedMerkleTree};
    use zksync_os_storage_api::{BlockContext, ReplayRecord};
    use zksync_os_types::{BlockStartCursors, ProtocolSemanticVersion};

    fn tree_block(block_number: u64) -> TreeBlock {
        let header = Header {
            number: block_number,
            timestamp: block_number,
            ..Default::default()
        }
        .seal_slow();
        let output = BlockOutput {
            header,
            tx_results: vec![],
            storage_writes: vec![],
            account_diffs: vec![],
            published_preimages: vec![],
            pubdata: BlockPubdata::new(0),
            computational_native_used: 0,
        };
        let record = ReplayRecord::new(
            BlockContext {
                block_number,
                timestamp: block_number,
                ..Default::default()
            },
            vec![],
            block_number.saturating_sub(1),
            semver::Version::new(0, 0, 0),
            ProtocolSemanticVersion::new(0, 32, 0),
            B256::ZERO,
            vec![],
            BlockStartCursors::default(),
        );
        let tree_output = TreeBatchOutput {
            root_hash: B256::ZERO,
            leaf_count: 2,
        };
        let tree = BlockMerkleTreeData {
            input: tree_output,
            output: tree_output,
            written_keys: vec![],
            read_keys: vec![],
            proof: BatchTreeProof {
                operations: vec![],
                read_operations: vec![],
                sorted_leaves: BTreeMap::new(),
                hashes: vec![],
            },
        };
        TreeBlock {
            output,
            record,
            tree,
        }
    }

    fn extend_with_tree_data(
        tree: &mut MerkleTree<RocksDBWrapper>,
        entries: &[TreeEntry],
        read_keys: &[B256],
    ) -> BlockMerkleTreeData {
        let previous_version = tree.latest_version().unwrap().unwrap();
        let (input_root, input_leaf_count) = tree.root_info(previous_version).unwrap().unwrap();
        let (output, proof) = tree.extend_with_proof(entries, read_keys).unwrap();
        BlockMerkleTreeData {
            input: TreeBatchOutput {
                root_hash: input_root,
                leaf_count: input_leaf_count,
            },
            output,
            written_keys: entries.iter().map(|entry| entry.key).collect(),
            read_keys: read_keys.to_vec(),
            proof,
        }
    }

    fn assert_raw_proof_eq(left: RawLeafProof, right: RawLeafProof) {
        assert_eq!(left.index, right.index);
        assert_eq!(left.key, right.key);
        assert_eq!(left.value, right.value);
        assert_eq!(left.next_index, right.next_index);
        assert_eq!(left.path, right.path);
    }

    fn assert_tree_adapter_equivalent(
        full: BlockMerkleTreeData,
        compact: BlockMerkleTreeData,
        tree: MerkleTree<RocksDBWrapper>,
        version: u64,
        existing_keys: &[B256],
        missing_keys: &[B256],
    ) {
        let mut full =
            EfficientTreeAdapter::new(full, VersionedMerkleTree::new(tree.clone(), version));
        let mut compact =
            EfficientTreeAdapter::new(compact, VersionedMerkleTree::new(tree, version));
        for &key in existing_keys {
            assert_eq!(full.read(key), compact.read(key));
            let full_index = full.tree_index(key).unwrap();
            assert_eq!(Some(full_index), compact.tree_index(key));
            assert_raw_proof_eq(
                full.merkle_proof(full_index),
                compact.merkle_proof(full_index),
            );
        }
        for &key in missing_keys {
            assert_eq!(full.read(key), compact.read(key));
            assert_eq!(full.tree_index(key), compact.tree_index(key));
            let full_previous = full.prev_tree_index(key);
            assert_eq!(full_previous, compact.prev_tree_index(key));
            assert_raw_proof_eq(
                full.merkle_proof(full_previous),
                compact.merkle_proof(full_previous),
            );
        }
    }

    async fn wait_for_staged_count(storage: &BatchWorkStorage, expected: usize) {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if storage.next_work_id.load(Ordering::Acquire) == expected as u64
                    && staged_file_count(storage).await == expected
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dispatcher did not durably stage expected work");
    }

    async fn staged_file_count(storage: &BatchWorkStorage) -> usize {
        let mut entries = fs::read_dir(&storage.base_dir).await.unwrap();
        let marker = format!("_{}_", storage.run_nonce);
        let mut count = 0;
        while let Some(entry) = entries.next_entry().await.unwrap() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            count += usize::from(name.contains(&marker) && name.ends_with(".json"));
        }
        count
    }

    // SYSCOIN: Target-gated compact recovery must be semantically identical to the normal batched
    // proof path for updates, insertions, and read-only keys across consecutive tree versions.
    #[test]
    fn compact_tree_fallback_matches_full_proof_across_sequential_blocks() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut tree =
            MerkleTree::new(RocksDBWrapper::new(&temp_dir.path().join("tree")).unwrap()).unwrap();
        let updated = B256::repeat_byte(0x10);
        let inserted = B256::repeat_byte(0x20);
        let read_only = B256::repeat_byte(0x30);
        let inserted_next = B256::repeat_byte(0x40);
        tree.extend(&[
            TreeEntry {
                key: updated,
                value: B256::repeat_byte(0xa1),
            },
            TreeEntry {
                key: read_only,
                value: B256::repeat_byte(0xb1),
            },
        ])
        .unwrap();

        let full_first = extend_with_tree_data(
            &mut tree,
            &[
                TreeEntry {
                    key: updated,
                    value: B256::repeat_byte(0xa2),
                },
                TreeEntry {
                    key: inserted,
                    value: B256::repeat_byte(0xc1),
                },
            ],
            &[read_only],
        );
        let full_second = extend_with_tree_data(
            &mut tree,
            &[
                TreeEntry {
                    key: inserted,
                    value: B256::repeat_byte(0xc2),
                },
                TreeEntry {
                    key: inserted_next,
                    value: B256::repeat_byte(0xd1),
                },
            ],
            &[updated],
        );
        let storage = BatchWorkStorage::new(temp_dir.path().join("queue")).unwrap();
        let (_sender, receiver) = mpsc::channel(1);
        let source = BatchWorkSource::new(storage, receiver, tree.clone());
        let compact_first = source.empty_tree_data(1).unwrap();
        let compact_second = source.empty_tree_data(2).unwrap();

        let encoded = serde_json::to_vec(&Some(full_first.clone())).unwrap();
        let decoded: Option<BlockMerkleTreeData> = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(encoded, serde_json::to_vec(&decoded).unwrap());
        assert_tree_adapter_equivalent(
            full_first,
            compact_first,
            tree.clone(),
            0,
            &[updated, read_only],
            &[inserted],
        );
        assert_tree_adapter_equivalent(
            full_second,
            compact_second,
            tree,
            1,
            &[inserted, updated],
            &[inserted_next],
        );
    }

    // SYSCOIN: More startup work than the bounded delivery capacity must reach disk without
    // blocking the rebuild frontier, and no handle may reach the batcher before authentication.
    #[tokio::test]
    async fn pending_target_stages_past_capacity_then_releases_exactly_once_in_order() {
        let temp_dir = tempfile::tempdir().unwrap();
        let storage = BatchWorkStorage::new(temp_dir.path()).unwrap();
        storage.begin_unauthenticated_epoch(3).unwrap();
        let (handle_tx, mut handle_rx) = mpsc::channel(1);
        let (target_tx, target_rx) = watch::channel(None);
        let dispatcher = BatchWorkDispatcher::new(storage.clone(), handle_tx, Some(target_rx));
        let (input_tx, input_rx) = mpsc::channel(8);
        let (output_tx, _output_rx) = mpsc::channel(1);
        let (state_reporter, _state_rx) =
            ComponentStateReporter::new("batch_work_target_gate_test");
        let run = tokio::spawn(dispatcher.run(
            PeekableReceiver::new(input_rx),
            output_tx,
            state_reporter,
        ));

        for block_number in 1..=3 {
            input_tx.send(tree_block(block_number)).await.unwrap();
        }
        wait_for_staged_count(&storage, 3).await;
        assert!(matches!(handle_rx.try_recv(), Err(TryRecvError::Empty)));

        target_tx.send_replace(Some(SYSCOIN_COMPACT_EDGE_DA_COMMIT_TARGET));
        for (expected_work_id, expected_block_number) in (0..3).zip(1..=3) {
            let handle = tokio::time::timeout(std::time::Duration::from_secs(1), handle_rx.recv())
                .await
                .expect("authenticated staged handle was not released")
                .expect("dispatcher dropped the handle channel");
            assert_eq!(handle.work_id, expected_work_id);
            assert_eq!(handle.block_number, expected_block_number);
        }
        assert!(matches!(handle_rx.try_recv(), Err(TryRecvError::Empty)));

        input_tx.send(tree_block(4)).await.unwrap();
        let live_handle = tokio::time::timeout(std::time::Duration::from_secs(1), handle_rx.recv())
            .await
            .expect("post-auth work did not use the live bounded channel")
            .expect("dispatcher dropped the handle channel");
        assert_eq!(live_handle.work_id, 3);
        assert_eq!(live_handle.block_number, 4);

        drop(input_tx);
        tokio::time::timeout(std::time::Duration::from_secs(1), run)
            .await
            .expect("dispatcher did not stop after its input closed")
            .unwrap()
            .unwrap();
        assert!(
            !temp_dir
                .path()
                .join(UNAUTHENTICATED_EPOCH_FRONTIER_FILE)
                .exists(),
            "exact target authentication must durably clear the epoch"
        );
    }

    // SYSCOIN: A restart at the cumulative cap may replay its durable frontier, but the next live
    // payload is held before canonization/persistence until exact target authentication arrives.
    #[tokio::test]
    async fn pre_persistence_restart_cap_holds_next_live_block_until_authentication() {
        let temp_dir = tempfile::tempdir().unwrap();
        let first_run = BatchWorkStorage::new(temp_dir.path()).unwrap();
        assert_eq!(first_run.begin_unauthenticated_epoch(0).unwrap(), 0);
        drop(first_run);
        let storage = BatchWorkStorage::new(temp_dir.path()).unwrap();
        let epoch_frontier = storage.begin_unauthenticated_epoch(2).unwrap();
        assert_eq!(epoch_frontier, 0, "restart must retain the original epoch");
        let (target_tx, target_rx) = watch::channel(None);
        let mut gate = BatchWorkPersistenceGate::new_with_unauthenticated_live_limit(
            storage.clone(),
            target_rx,
            2,
            epoch_frontier,
            2,
            Duration::from_millis(10),
        );

        gate.admit_block(BlockCommandType::Produce, 2)
            .await
            .unwrap();
        let error = gate
            .admit_block(BlockCommandType::Produce, 3)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("live batch-work limit 2"));
        assert!(
            temp_dir
                .path()
                .join(UNAUTHENTICATED_EPOCH_FRONTIER_FILE)
                .exists(),
            "authentication timeout must retain the epoch"
        );

        target_tx.send_replace(Some(SYSCOIN_COMPACT_EDGE_DA_COMMIT_TARGET));
        gate.admit_block(BlockCommandType::Produce, 3)
            .await
            .unwrap();
        assert!(
            !temp_dir
                .path()
                .join(UNAUTHENTICATED_EPOCH_FRONTIER_FILE)
                .exists(),
            "exact authentication must release the held payload and clear the epoch"
        );
    }

    // SYSCOIN: A follower must persist already-canonical Raft replays, including the block that
    // installs the target. Replays saturate the cumulative epoch so later local leadership still
    // cannot create an extra unauthenticated allowance.
    #[tokio::test]
    async fn post_startup_replays_saturate_without_blocking_persistence() {
        let temp_dir = tempfile::tempdir().unwrap();
        let storage = BatchWorkStorage::new(temp_dir.path()).unwrap();
        let epoch_frontier = storage.begin_unauthenticated_epoch(0).unwrap();
        let (target_tx, target_rx) = watch::channel(None);
        let mut gate = BatchWorkPersistenceGate::new_with_unauthenticated_live_limit(
            storage,
            target_rx,
            0,
            epoch_frontier,
            2,
            Duration::from_millis(10),
        );

        for block_number in 1..=3 {
            gate.admit_block(BlockCommandType::Replay, block_number)
                .await
                .unwrap();
        }
        assert_eq!(gate.unauthenticated_live_batch_work, 2);
        let error = gate
            .admit_block(BlockCommandType::Produce, 4)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("live batch-work limit 2"));

        target_tx.send_replace(Some(SYSCOIN_COMPACT_EDGE_DA_COMMIT_TARGET));
        gate.admit_block(BlockCommandType::Produce, 4)
            .await
            .unwrap();
    }

    // SYSCOIN: Runtime rebuilds are not a valid way to propose beyond the startup WAL frontier,
    // regardless of whether the compact target is already authenticated.
    #[tokio::test]
    async fn post_frontier_rebuild_is_rejected_after_authentication() {
        let temp_dir = tempfile::tempdir().unwrap();
        let storage = BatchWorkStorage::new(temp_dir.path()).unwrap();
        let epoch_frontier = storage.begin_unauthenticated_epoch(0).unwrap();
        let (_target_tx, target_rx) = watch::channel(Some(SYSCOIN_COMPACT_EDGE_DA_COMMIT_TARGET));
        let mut gate = BatchWorkPersistenceGate::new_with_unauthenticated_live_limit(
            storage,
            target_rx,
            0,
            epoch_frontier,
            2,
            Duration::from_millis(10),
        );

        let error = gate
            .admit_block(BlockCommandType::Rebuild, 1)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("post-frontier rebuild block 1"));
    }

    // SYSCOIN: The durable compact representation must retain the status gate used by both
    // interop and forwarded Edge-DA log collection.
    #[test]
    fn batch_work_roundtrip_preserves_transaction_success_status() {
        for execution_result in [
            ExecutionResult::Success(ExecutionOutput::Call(Vec::new())),
            ExecutionResult::Revert(Vec::new()),
        ] {
            let expected_success = matches!(execution_result, ExecutionResult::Success(_));
            let output = TxOutput {
                execution_result,
                gas_used: 0,
                gas_refunded: 0,
                computational_native_used: 0,
                native_used: 0,
                pubdata_used: 0,
                contract_address: None,
                logs: Vec::new(),
                l2_to_l1_logs: Vec::new(),
                storage_writes: Vec::new(),
            };
            let reconstructed = BatchWorkTxOutput::from(output).into_tx_output();
            assert_eq!(reconstructed.is_success(), expected_success);
        }
    }

    // SYSCOIN: Native validation must use the preserved replay hash, not re-hash
    // a staged output whose gas and storage writes were deliberately omitted.
    #[tokio::test]
    async fn batch_work_disk_roundtrip_preserves_full_replay_hash_not_full_output() {
        let temp_dir = tempfile::tempdir().unwrap();
        let storage = BatchWorkStorage::new(temp_dir.path()).unwrap();
        let TreeBlock {
            mut output,
            mut record,
            tree,
        } = tree_block(1);
        let mut tx = BatchWorkTxOutput {
            success: true,
            l2_to_l1_logs: vec![],
        }
        .into_tx_output();
        tx.gas_used = 21_000;
        output.tx_results.push(Ok(tx));
        output.storage_writes.push(StorageWrite {
            key: B256::repeat_byte(0x11),
            value: B256::repeat_byte(0x22),
            account: Address::ZERO,
            account_key: B256::ZERO,
        });
        record.block_output_hash = zksync_os_types::block_output_hash(
            output.header.hash(),
            &output.tx_results,
            &output.storage_writes,
        );
        let replay_hash = record.block_output_hash;
        let header_hash = output.header.hash();
        let handle = storage.store(output, record, Some(tree)).await.unwrap();
        let (compact, replay, _) = storage.load(&handle).await.unwrap().into_parts();
        assert_eq!(replay.block_output_hash, replay_hash);
        assert_eq!(compact.header.hash(), header_hash);
        assert_eq!(compact.tx_results[0].as_ref().unwrap().gas_used, 0);
        assert!(compact.storage_writes.is_empty());
        assert_ne!(
            zksync_os_types::block_output_hash(
                compact.header.hash(),
                &compact.tx_results,
                &compact.storage_writes,
            ),
            replay_hash
        );
    }

    // SYSCOIN: Even an arbitrary canonical rebuild must fail before its compact sidecar can grow
    // past the explicit disk budget; a rejected item leaves neither accounting nor a partial file.
    #[tokio::test]
    async fn staging_byte_limit_rejects_before_writing() {
        let temp_dir = tempfile::tempdir().unwrap();
        let storage = BatchWorkStorage::new_with_staging_limit(temp_dir.path(), 1).unwrap();
        let TreeBlock { output, record, .. } = tree_block(1);
        let error = storage.store(output, record, None).await.unwrap_err();
        assert!(error.to_string().contains("staging limit 1 bytes"));
        assert_eq!(storage.next_work_id.load(Ordering::Acquire), 0);
        assert_eq!(storage.staged_bytes.load(Ordering::Acquire), 0);
        assert_eq!(staged_file_count(&storage).await, 0);
    }

    // SYSCOIN: Neither a dead publisher nor an arbitrary non-app address may release staged work.
    #[tokio::test]
    async fn pending_target_fails_closed_on_closed_or_wrong_source() {
        for source in [
            {
                let (sender, receiver) = watch::channel(None);
                drop(sender);
                receiver
            },
            {
                let (_sender, receiver) = watch::channel(Some(Address::repeat_byte(0x11)));
                receiver
            },
        ] {
            let temp_dir = tempfile::tempdir().unwrap();
            let storage = BatchWorkStorage::new(temp_dir.path()).unwrap();
            storage.begin_unauthenticated_epoch(0).unwrap();
            let (handle_tx, _handle_rx) = mpsc::channel(1);
            let dispatcher = BatchWorkDispatcher::new(storage, handle_tx, Some(source));
            let (_input_tx, input_rx) = mpsc::channel(1);
            let (output_tx, _output_rx) = mpsc::channel(1);
            let (state_reporter, _state_rx) =
                ComponentStateReporter::new("batch_work_target_failure_test");
            let error = dispatcher
                .run(PeekableReceiver::new(input_rx), output_tx, state_reporter)
                .await
                .unwrap_err();
            assert!(
                error.to_string().contains("target"),
                "unexpected fail-closed error: {error}"
            );
            assert!(
                temp_dir
                    .path()
                    .join(UNAUTHENTICATED_EPOCH_FRONTIER_FILE)
                    .exists(),
                "wrong or closed target must retain the epoch"
            );
        }
    }
}
