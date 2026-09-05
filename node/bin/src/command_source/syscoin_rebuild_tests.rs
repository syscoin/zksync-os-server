//! SYSCOIN: Follower rebuild regressions through the actual source, VM, canonizer and applier.
use super::ConsensusNodeCommandSource;
use alloy::network::EthereumWallet;
use alloy::primitives::{Address, B256, BlockHash, Sealed, TxHash, U256};
use alloy::providers::ProviderBuilder;
use alloy::rpc::json_rpc::ErrorPayload;
use alloy::transports::mock::Asserter;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use zksync_os_contract_interface::{L2CanonicalTransaction, ZkChain};
use zksync_os_genesis::{Genesis, GenesisInput, GenesisInputSource, GenesisUpgradeTxInfo};
use zksync_os_interface::error::InvalidTransaction;
use zksync_os_interface::tracing::{NopTracer, NopValidator};
use zksync_os_interface::traits::{PreimageSource, ReadStorage};
use zksync_os_interface::types::StorageWrite;
use zksync_os_mempool::MarkingTxStream;
use zksync_os_observability::ComponentStateReporter;
use zksync_os_pipeline::{PeekableReceiver, PipelineComponent};
use zksync_os_provider::NodeProvider;
use zksync_os_sequencer::config::SequencerConfig;
use zksync_os_sequencer::execution::BlockApplier;
use zksync_os_sequencer::execution::block_canonizer::{BlockCanonization, BlockCanonizer};
use zksync_os_sequencer::execution::execute_block_in_vm::execute_block_in_vm;
use zksync_os_sequencer::model::blocks::{
    BlockCommand, BlockCommandType, BlockOutputWithReads, BlockPayload, InvalidTxPolicy,
    PreparedBlockCommand, SealPolicy,
};
use zksync_os_state_full_diffs::FullDiffsState;
use zksync_os_storage::db::BlockReplayStorage;
use zksync_os_storage_api::{
    BlockContext, BlockHashes, LogIndex, ReadReplay, ReadRepository, ReadStateHistory,
    ReplayRecord, RepositoryBlock, RepositoryResult, StoredTxData, TxMeta, WriteReplay,
    WriteRepository, WriteState,
};
use zksync_os_types::{
    BlockOutput, BlockStartCursors, NodeRole, ProtocolSemanticVersion, ZkReceiptEnvelope,
    ZkTransaction,
};

#[derive(Clone)]
struct EmptyState;

impl ReadStorage for EmptyState {
    fn read(&mut self, _key: B256) -> Option<B256> {
        None
    }
}

impl PreimageSource for EmptyState {
    fn get_preimage(&mut self, _hash: B256) -> Option<Vec<u8>> {
        None
    }
}

#[derive(Clone)]
struct ObserveState {
    overrides: Arc<Mutex<Vec<bool>>>,
    inner: FullDiffsState,
}

#[derive(Debug, Clone)]
struct ObserveReplay {
    inner: BlockReplayStorage,
    writes: Arc<Mutex<Vec<(bool, bool)>>>,
}

impl ReadReplay for ObserveReplay {
    fn get_context(&self, block_number: u64) -> Option<BlockContext> {
        self.inner.get_context(block_number)
    }

    fn get_original_context(&self, block_number: u64) -> Option<BlockContext> {
        self.inner.get_original_context(block_number)
    }

    fn get_replay_record_identity(&self, block_number: u64) -> Option<BlockHash> {
        self.inner.get_replay_record_identity(block_number)
    }

    fn get_replay_record_by_key(
        &self,
        block_number: u64,
        db_key: Option<Vec<u8>>,
    ) -> Option<ReplayRecord> {
        self.inner.get_replay_record_by_key(block_number, db_key)
    }

    fn get_canonical_block_hash(&self, block_number: u64) -> Option<BlockHash> {
        self.inner.get_canonical_block_hash(block_number)
    }

    fn latest_record(&self) -> u64 {
        self.inner.latest_record()
    }
}

impl WriteReplay for ObserveReplay {
    async fn write(
        &self,
        record: Sealed<ReplayRecord>,
        override_allowed: bool,
    ) -> anyhow::Result<bool> {
        let written = self.inner.write(record, override_allowed).await?;
        self.writes
            .lock()
            .unwrap()
            .push((override_allowed, written));
        Ok(written)
    }
}

impl WriteState for ObserveState {
    fn add_block_result<'a, J>(
        &self,
        block: u64,
        writes: Vec<StorageWrite>,
        preimages: J,
        allow: bool,
    ) -> anyhow::Result<()>
    where
        J: IntoIterator<Item = (B256, &'a Vec<u8>)>,
    {
        self.overrides.lock().unwrap().push(allow);
        self.inner.add_block_result(block, writes, preimages, allow)
    }
}

#[derive(Debug)]
struct EmptyGenesisInput;

#[async_trait::async_trait]
impl GenesisInputSource for EmptyGenesisInput {
    async fn genesis_input(&self) -> anyhow::Result<GenesisInput> {
        Ok(GenesisInput {
            initial_contracts: vec![],
            additional_storage: Default::default(),
            additional_storage_raw: vec![],
            additional_preimages: vec![],
            genesis_root: B256::ZERO,
        })
    }
}

async fn state_fixture(path: &Path) -> ObserveState {
    // SYSCOIN: Genesis is injected locally; the mocked provider only answers capability probes,
    // so these real RocksDB state regressions never need a live L1 connection.
    let asserter = Asserter::new();
    let capability: alloy::rpc::types::Header = Default::default();
    asserter.push_success(&capability);
    asserter.push_success(&capability);
    asserter.push_failure(ErrorPayload::method_not_found());
    asserter.push_success(&"anvil/v1.0.0".to_owned());
    let provider = ProviderBuilder::new()
        .disable_recommended_fillers()
        .wallet(EthereumWallet::default())
        .connect_mocked_client(asserter);
    let provider = NodeProvider::new(provider).await.unwrap();
    let genesis = Genesis::new_with_authenticated_genesis_upgrade(
        Arc::new(EmptyGenesisInput),
        ZkChain::new(Address::ZERO, provider),
        270,
        GenesisUpgradeTxInfo {
            protocol_version: ProtocolSemanticVersion::canonical_genesis_version(),
            tx: L2CanonicalTransaction {
                txType: U256::from(0x7e),
                from: U256::ZERO,
                to: U256::ZERO,
                gasLimit: U256::ZERO,
                gasPerPubdataByteLimit: U256::ZERO,
                maxFeePerGas: U256::ZERO,
                maxPriorityFeePerGas: U256::ZERO,
                paymaster: U256::ZERO,
                nonce: U256::ZERO,
                value: U256::ZERO,
                reserved: [U256::ZERO; 4],
                data: Default::default(),
                signature: Default::default(),
                factoryDeps: vec![],
                paymasterInput: Default::default(),
                reservedDynamic: Default::default(),
            }
            .try_into()
            .unwrap(),
            force_deploy_preimages: vec![],
        },
    );
    ObserveState {
        overrides: Default::default(),
        inner: FullDiffsState::new(path.to_path_buf(), &genesis)
            .await
            .unwrap(),
    }
}

fn storage_write(key: u8, value: u8) -> StorageWrite {
    StorageWrite {
        key: B256::repeat_byte(key),
        value: B256::repeat_byte(value),
        account: Default::default(),
        account_key: Default::default(),
    }
}

struct CountingCanonization {
    proposals: Arc<AtomicUsize>,
    sender: mpsc::UnboundedSender<ReplayRecord>,
    receiver: mpsc::UnboundedReceiver<ReplayRecord>,
}

#[async_trait::async_trait]
impl BlockCanonization for CountingCanonization {
    async fn propose(&self, record: ReplayRecord) -> anyhow::Result<()> {
        self.proposals.fetch_add(1, Ordering::Relaxed);
        self.sender.send(record)?;
        Ok(())
    }

    async fn next_canonized(&mut self) -> anyhow::Result<ReplayRecord> {
        self.receiver
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("consensus closed"))
    }
}

async fn canonize(payload: BlockPayload, expect_proposal: bool) -> BlockPayload {
    let proposals = Arc::new(AtomicUsize::new(0));
    let (sender, receiver) = mpsc::unbounded_channel();
    let (canonized_blocks_for_execution, mut reexecute) = mpsc::unbounded_channel();
    let canonizer = BlockCanonizer {
        consensus: CountingCanonization {
            proposals: proposals.clone(),
            sender,
            receiver,
        },
        canonized_blocks_for_execution,
    };
    let (input_sender, input_receiver) = mpsc::channel(1);
    let (output_sender, mut output_receiver) = mpsc::channel(1);
    let (reporter, _) = ComponentStateReporter::new("syscoin_rebuild_canonizer");
    let task = tokio::spawn(canonizer.run(
        PeekableReceiver::new(input_receiver),
        output_sender,
        reporter,
    ));
    input_sender.send(payload).await.unwrap();
    let payload = tokio::time::timeout(Duration::from_secs(10), output_receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        proposals.load(Ordering::Relaxed),
        usize::from(expect_proposal)
    );
    assert!(
        reexecute.try_recv().is_err(),
        "already-executed replay was requeued"
    );
    drop(input_sender);
    task.await.unwrap().unwrap();
    payload
}

#[derive(Debug, Clone, Default)]
struct ObserveRepository(Arc<AtomicUsize>);

impl LogIndex for ObserveRepository {}

impl ReadRepository for ObserveRepository {
    fn get_block_by_number(&self, _n: u64) -> RepositoryResult<Option<RepositoryBlock>> {
        Ok(None)
    }
    fn get_block_by_hash(&self, _h: BlockHash) -> RepositoryResult<Option<RepositoryBlock>> {
        Ok(None)
    }
    fn get_raw_transaction(&self, _h: TxHash) -> RepositoryResult<Option<Vec<u8>>> {
        Ok(None)
    }
    fn get_transaction(&self, _h: TxHash) -> RepositoryResult<Option<ZkTransaction>> {
        Ok(None)
    }
    fn get_transaction_receipt(&self, _h: TxHash) -> RepositoryResult<Option<ZkReceiptEnvelope>> {
        Ok(None)
    }
    fn get_transaction_meta(&self, _h: TxHash) -> RepositoryResult<Option<TxMeta>> {
        Ok(None)
    }
    fn get_transaction_hash_by_sender_nonce(
        &self,
        _a: Address,
        _n: u64,
    ) -> RepositoryResult<Option<TxHash>> {
        Ok(None)
    }
    fn get_stored_transaction(&self, _h: TxHash) -> RepositoryResult<Option<StoredTxData>> {
        Ok(None)
    }
    fn get_latest_block(&self) -> u64 {
        0
    }
}

impl WriteRepository for ObserveRepository {
    async fn populate(
        &self,
        _output: BlockOutput,
        _txs: Vec<ZkTransaction>,
        _failed: Vec<(TxHash, InvalidTransaction)>,
    ) -> RepositoryResult<()> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

async fn execute_empty(context: BlockContext) -> (BlockOutputWithReads, ReplayRecord) {
    let (reporter, _) = ComponentStateReporter::new("syscoin_rebuild_vm");
    let command = PreparedBlockCommand {
        block_context: context,
        seal_policy: SealPolicy::UntilExhausted {
            allowed_to_finish_early: false,
        },
        invalid_tx_policy: InvalidTxPolicy::Abort,
        tx_source: MarkingTxStream::unmarkable(futures::stream::empty()),
        metrics_label: "syscoin_follower_rebuild",
        protocol_version: ProtocolSemanticVersion::canonical_genesis_version(),
        expected_block_output_hash: None,
        previous_block_timestamp: 0,
        force_preimages: vec![],
        expect_sl_chain_id_tx_after_upgrade: false,
        starting_cursors: BlockStartCursors::default(),
        interop_roots_per_block: 10,
        strict_subpool_cleanup: false,
    };
    let (output, record, rejected, _) =
        execute_block_in_vm(command, EmptyState, &reporter, NopTracer, NopValidator)
            .await
            .unwrap_or_else(|dump| panic!("empty fixture failed: {}", dump.error));
    assert!(rejected.is_empty());
    (output, record)
}

fn block_context() -> BlockContext {
    BlockContext {
        chain_id: 270,
        block_number: 1,
        timestamp: 1_000,
        block_hashes: BlockHashes::default().push(zksync_os_genesis::genesis_header().hash()),
        eip1559_basefee: U256::from(100),
        native_price: U256::from(100),
        pubdata_price: U256::from(100),
        gas_limit: 100_000_000,
        pubdata_limit: 1_000_000,
        execution_version: 7,
        blob_fee: U256::ONE,
        ..Default::default()
    }
}

fn applier_config(path: &Path, node_role: NodeRole) -> SequencerConfig {
    SequencerConfig {
        node_role,
        block_time: Duration::from_millis(250),
        max_transactions_in_block: 10,
        block_dump_path: path.join("dumps"),
        block_gas_limit: 100_000_000,
        block_pubdata_limit_bytes: 1_000_000,
        max_blocks_to_produce: None,
        interop_roots_per_tx: 10,
        tx_validator: Default::default(),
    }
}

async fn assert_replacement_is_persisted(command_type: BlockCommandType, node_role: NodeRole) {
    let dir = tempfile::tempdir().unwrap();
    let storage = BlockReplayStorage::new_without_genesis(dir.path(), 270);
    let genesis_hash = zksync_os_genesis::genesis_header().hash();
    let context = block_context();
    let (original_output, original) = execute_empty(context).await;
    let old_hash = original_output.as_ref().header.hash();
    let mut genesis = original.clone();
    genesis.block_context.block_number = 0;
    genesis.block_context.timestamp = 0;
    genesis.block_context.block_hashes = BlockHashes::default();
    assert!(
        storage
            .write(Sealed::new_unchecked(genesis, genesis_hash), false)
            .await
            .unwrap()
    );
    assert!(
        storage
            .write(Sealed::new_unchecked(original.clone(), old_hash), false)
            .await
            .unwrap()
    );

    // Resetting timestamps is a supported rebuild option; both payloads come from the real VM.
    let (output, replacement) = execute_empty(BlockContext {
        timestamp: 2_000,
        ..context
    })
    .await;
    let replacement_hash = output.as_ref().header.hash();
    assert_ne!(old_hash, replacement_hash);
    assert_ne!(original.block_output_hash, replacement.block_output_hash);

    let cmd_type = if matches!(command_type, BlockCommandType::CanonizedRebuild) {
        let (sender, mut receiver) = mpsc::channel(1);
        let (reporter, _) = ComponentStateReporter::new("syscoin_rebuild_source");
        let mut next = 1;
        assert!(
            ConsensusNodeCommandSource::<BlockReplayStorage>::forward_canonized_rebuild(
                replacement.clone(),
                &mut next,
                1,
                &sender,
                &reporter,
            )
            .await
            .unwrap()
        );
        let command = receiver.recv().await.unwrap();
        assert!(
            matches!(&command, BlockCommand::CanonizedRebuild(record) if record.as_ref() == &replacement)
        );
        command.command_type()
    } else {
        command_type
    };
    let override_expected =
        node_role.is_external() || !matches!(cmd_type, BlockCommandType::Replay);
    let (output, expected_record, expected_hash) = if override_expected {
        (output, replacement, replacement_hash)
    } else {
        // Ordinary startup replay remains idempotent in the WAL. Its authenticated output may
        // repair derived state after a crash between durable WAL and state writes.
        (original_output, original, old_hash)
    };
    let state = state_fixture(dir.path()).await;
    state
        .inner
        .add_block_result(0, vec![storage_write(1, 1)], std::iter::empty(), false)
        .unwrap();
    // SYSCOIN: Seed an independent removed write and suffix to exercise the real state store,
    // in addition to the actual VM-generated WAL replacement fixture.
    state
        .inner
        .add_block_result(1, vec![storage_write(1, 2)], std::iter::empty(), false)
        .unwrap();
    state
        .inner
        .add_block_result(2, vec![storage_write(2, 3)], std::iter::empty(), false)
        .unwrap();
    let payload = canonize(
        BlockPayload {
            output,
            record: expected_record.clone(),
            command_type: cmd_type,
            failed_transactions: vec![],
        },
        matches!(cmd_type, BlockCommandType::Rebuild),
    )
    .await;
    assert!(matches!(
        (cmd_type, payload.command_type),
        (
            BlockCommandType::CanonizedRebuild,
            BlockCommandType::CanonizedRebuild
        ) | (BlockCommandType::Rebuild, BlockCommandType::Rebuild)
            | (BlockCommandType::Replay, BlockCommandType::Replay)
    ));
    let (applied_sender, applied_receiver) = watch::channel(None);
    let replay = ObserveReplay {
        inner: storage.clone(),
        writes: Default::default(),
    };
    let applier = BlockApplier {
        state: state.clone(),
        replay: replay.clone(),
        repositories: ObserveRepository::default(),
        config: applier_config(dir.path(), node_role),
        applied_block_number_sender: applied_sender,
    };
    let (input_sender, input_receiver) = mpsc::channel(1);
    input_sender.send(payload).await.unwrap();
    drop(input_sender);
    let (output_sender, mut output_receiver) = mpsc::channel(1);
    let (reporter, _) = ComponentStateReporter::new("syscoin_rebuild_applier");
    applier
        .run(
            PeekableReceiver::new(input_receiver),
            output_sender,
            reporter,
        )
        .await
        .unwrap();
    assert_eq!(*applied_receiver.borrow(), Some(1));
    assert_eq!(
        output_receiver.recv().await.unwrap().record,
        expected_record
    );
    let override_flags = state.overrides.lock().unwrap().clone();
    assert_eq!(override_flags, vec![true]);
    assert_eq!(
        *replay.writes.lock().unwrap(),
        vec![(override_expected, override_expected)]
    );
    assert_eq!(
        storage.get_canonical_block_hash(1),
        Some(expected_hash),
        "applier reported the rebuilt block applied but its canonical WAL hash is stale"
    );
    assert_eq!(storage.get_replay_record(1), Some(expected_record.clone()));
    let mut view = state.inner.state_view_at(1).unwrap();
    assert_eq!(view.read(B256::repeat_byte(1)), Some(B256::repeat_byte(1)));
    assert_eq!(*state.inner.block_range_available().end(), 1);
    drop(view);

    if override_expected {
        // The exact permission passed by the applier must also accept changed historical writes.
        // An ordinary main-node Replay used to pass false and panic on this operation.
        state
            .inner
            .add_block_result(
                1,
                vec![storage_write(1, 4)],
                std::iter::empty(),
                override_flags[0],
            )
            .unwrap();
        let mut next_base = state.inner.state_view_at(1).unwrap();
        assert_eq!(
            next_base.read(B256::repeat_byte(1)),
            Some(B256::repeat_byte(4))
        );
        assert_eq!(
            next_base.read(B256::repeat_byte(2)),
            None,
            "stale suffix survived rebuild"
        );

        // SYSCOIN: Execute the next block against the replacement base, not an overlay on the
        // old suffix; the executor normally waits for this same applier boundary.
        let (reporter, _) = ComponentStateReporter::new("syscoin_rebuild_next_block");
        let prepared = PreparedBlockCommand {
            block_context: BlockContext {
                block_number: 2,
                timestamp: expected_record.block_context.timestamp + 1,
                block_hashes: expected_record
                    .block_context
                    .block_hashes
                    .push(expected_hash),
                ..expected_record.block_context
            },
            seal_policy: SealPolicy::UntilExhausted {
                allowed_to_finish_early: false,
            },
            invalid_tx_policy: InvalidTxPolicy::Abort,
            tx_source: MarkingTxStream::unmarkable(futures::stream::empty()),
            metrics_label: "syscoin_rebuild_next_block",
            protocol_version: expected_record.protocol_version,
            expected_block_output_hash: None,
            previous_block_timestamp: expected_record.block_context.timestamp,
            force_preimages: vec![],
            expect_sl_chain_id_tx_after_upgrade: false,
            starting_cursors: expected_record.starting_cursors,
            interop_roots_per_block: 10,
            strict_subpool_cleanup: false,
        };
        let (_, next_record, _, _) =
            execute_block_in_vm(prepared, next_base, &reporter, NopTracer, NopValidator)
                .await
                .unwrap_or_else(|dump| panic!("next block failed: {}", dump.error));
        assert_eq!(next_record.block_context.block_number, 2);
        assert_eq!(
            next_record.block_context.block_hashes.0[255],
            U256::from_be_slice(expected_hash.as_slice())
        );
    }
}

#[tokio::test]
async fn leader_rebuild_replaces_existing_wal() {
    assert_replacement_is_persisted(BlockCommandType::Rebuild, NodeRole::MainNode).await;
}

#[tokio::test]
async fn follower_rebuild_replaces_existing_wal() {
    assert_replacement_is_persisted(BlockCommandType::CanonizedRebuild, NodeRole::MainNode).await;
}

#[tokio::test]
async fn ordinary_main_node_replay_only_repairs_derived_state() {
    assert_replacement_is_persisted(BlockCommandType::Replay, NodeRole::MainNode).await;
}

#[tokio::test]
async fn restart_after_durable_rebuild_wal_repairs_stale_state_without_wal_rewrite() {
    // SYSCOIN: Crash exactly after the replacement WAL row is durable but before state is
    // updated. The restart now sees a complete rebuilt WAL and uses ordinary strict replay.
    let dir = tempfile::tempdir().unwrap();
    let storage = BlockReplayStorage::new_without_genesis(dir.path(), 270);
    let state = state_fixture(dir.path()).await;
    let (old_output, old_record) = execute_empty(block_context()).await;
    let old_hash = old_output.as_ref().header.hash();
    let mut genesis = old_record.clone();
    genesis.block_context.block_number = 0;
    genesis.block_context.timestamp = 0;
    genesis.block_context.block_hashes = BlockHashes::default();
    storage
        .write(
            Sealed::new_unchecked(genesis, zksync_os_genesis::genesis_header().hash()),
            false,
        )
        .await
        .unwrap();
    storage
        .write(Sealed::new_unchecked(old_record, old_hash), false)
        .await
        .unwrap();
    state
        .inner
        .add_block_result(
            0,
            vec![storage_write(1, 1), storage_write(3, 7)],
            std::iter::empty(),
            false,
        )
        .unwrap();
    // Independent old writes cover a changed existing value and a newly-created key that the
    // canonical replacement removes, without requiring a deployed-contract genesis fixture.
    state
        .inner
        .add_block_result(
            1,
            vec![storage_write(1, 2), storage_write(2, 3)],
            std::iter::empty(),
            false,
        )
        .unwrap();
    let (replacement_output, replacement_record) = execute_empty(BlockContext {
        timestamp: 2_000,
        ..block_context()
    })
    .await;
    let replacement_hash = replacement_output.as_ref().header.hash();
    assert!(
        storage
            .write(
                Sealed::new_unchecked(replacement_record.clone(), replacement_hash),
                true
            )
            .await
            .unwrap()
    );
    // Both real stores are closed and reopened; no in-memory overlay can repair this fixture.
    drop(storage);
    drop(state);

    let storage = BlockReplayStorage::new_without_genesis(dir.path(), 270);
    let state = state_fixture(dir.path()).await;
    let persisted = storage.get_replay_record(1).unwrap();
    assert_eq!(persisted, replacement_record);
    let mut stale_view = state.inner.state_view_at(1).unwrap();
    assert_eq!(
        stale_view.read(B256::repeat_byte(1)),
        Some(B256::repeat_byte(2))
    );
    assert_eq!(
        stale_view.read(B256::repeat_byte(2)),
        Some(B256::repeat_byte(3))
    );
    drop(stale_view);

    let prepared = PreparedBlockCommand {
        block_context: persisted.block_context,
        seal_policy: SealPolicy::UntilExhausted {
            allowed_to_finish_early: false,
        },
        invalid_tx_policy: InvalidTxPolicy::Abort,
        tx_source: MarkingTxStream::unmarkable(futures::stream::iter(
            persisted.transactions.clone(),
        )),
        metrics_label: "syscoin_rebuild_state_recovery",
        protocol_version: persisted.protocol_version.clone(),
        expected_block_output_hash: Some(persisted.block_output_hash),
        previous_block_timestamp: persisted.previous_block_timestamp,
        force_preimages: persisted.force_preimages.clone(),
        expect_sl_chain_id_tx_after_upgrade: false,
        starting_cursors: persisted.starting_cursors.clone(),
        interop_roots_per_block: 10,
        strict_subpool_cleanup: false,
    };
    let (reporter, _) = ComponentStateReporter::new("syscoin_rebuild_state_recovery");
    let (output, record, _, _) = execute_block_in_vm(
        prepared,
        state.inner.state_view_at(0).unwrap(),
        &reporter,
        NopTracer,
        NopValidator,
    )
    .await
    .unwrap_or_else(|dump| panic!("strict restart replay failed: {}", dump.error));
    assert_eq!(record, persisted);
    let payload = canonize(
        BlockPayload {
            output,
            record,
            command_type: BlockCommandType::Replay,
            failed_transactions: vec![],
        },
        false,
    )
    .await;
    let replay = ObserveReplay {
        inner: storage.clone(),
        writes: Default::default(),
    };
    let repository = ObserveRepository::default();
    let (applied_sender, applied_receiver) = watch::channel(None);
    let applier = BlockApplier {
        state: state.clone(),
        replay: replay.clone(),
        repositories: repository.clone(),
        config: applier_config(dir.path(), NodeRole::MainNode),
        applied_block_number_sender: applied_sender,
    };
    let (input_sender, input_receiver) = mpsc::channel(1);
    input_sender.send(payload).await.unwrap();
    drop(input_sender);
    let (output_sender, mut output_receiver) = mpsc::channel(1);
    applier
        .run(
            PeekableReceiver::new(input_receiver),
            output_sender,
            reporter,
        )
        .await
        .unwrap();

    assert_eq!(*replay.writes.lock().unwrap(), vec![(false, false)]);
    assert_eq!(*state.overrides.lock().unwrap(), vec![true]);
    assert_eq!(*applied_receiver.borrow(), Some(1));
    assert_eq!(repository.0.load(Ordering::Relaxed), 1);
    assert_eq!(
        output_receiver.recv().await.unwrap().record,
        replacement_record
    );
    assert_eq!(storage.get_canonical_block_hash(1), Some(replacement_hash));
    assert_eq!(storage.get_replay_record(1), Some(persisted));
    let mut repaired = state.inner.state_view_at(1).unwrap();
    assert_eq!(
        repaired.read(B256::repeat_byte(1)),
        Some(B256::repeat_byte(1))
    );
    assert_eq!(repaired.read(B256::repeat_byte(2)), None);
    assert_eq!(
        repaired.read(B256::repeat_byte(3)),
        Some(B256::repeat_byte(7))
    );
}

#[tokio::test]
async fn external_node_replay_keeps_overwrite_permission() {
    assert_replacement_is_persisted(BlockCommandType::Replay, NodeRole::ExternalNode).await;
}

#[tokio::test]
async fn follower_source_only_authorizes_the_next_configured_rebuild() {
    // SYSCOIN: Replacement permission is emitted only by the ordered rebuild path; ordinary
    // consensus traffic keeps Replay, and an out-of-order rebuild never enters the pipeline.
    let (_, record) = execute_empty(block_context()).await;
    let (sender, mut receiver) = mpsc::channel(1);
    let (reporter, _) = ComponentStateReporter::new("syscoin_rebuild_order");
    let mut next = 2;
    let error = ConsensusNodeCommandSource::<BlockReplayStorage>::forward_canonized_rebuild(
        record.clone(),
        &mut next,
        3,
        &sender,
        &reporter,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("out of order"));
    assert_eq!(next, 2);
    assert!(receiver.try_recv().is_err());

    assert!(
        ConsensusNodeCommandSource::<BlockReplayStorage>::forward_replay(
            record.clone(),
            &sender,
            &reporter,
        )
        .await
        .unwrap()
    );
    assert!(
        matches!(receiver.recv().await, Some(BlockCommand::Replay(replay)) if *replay == record)
    );

    next = 1;
    assert!(
        !ConsensusNodeCommandSource::<BlockReplayStorage>::forward_canonized_rebuild(
            record.clone(),
            &mut next,
            2,
            &sender,
            &reporter,
        )
        .await
        .unwrap()
    );
    assert_eq!(next, 2);
    assert!(
        matches!(receiver.recv().await, Some(BlockCommand::CanonizedRebuild(replay)) if *replay == record)
    );
}

#[tokio::test]
async fn archive_header_mismatch_stops_applier_before_any_publication() {
    // SYSCOIN: Archive bytes can contain a valid replay of a different terminal block while
    // claiming the trusted anchor's header hash. The VM output is real and its replay-output
    // hash is self-consistent; persistence must still reject the independently bound header.
    let dir = tempfile::tempdir().unwrap();
    let storage = BlockReplayStorage::new_without_genesis(dir.path(), 270);
    let context = block_context();
    let (trusted_output, _) = execute_empty(context).await;
    let trusted_hash = trusted_output.as_ref().header.hash();
    let (alternative_output, alternative_record) = execute_empty(BlockContext {
        timestamp: 2_000,
        ..context
    })
    .await;
    assert_ne!(alternative_output.as_ref().header.hash(), trusted_hash);
    let mut genesis = alternative_record.clone();
    genesis.block_context.block_number = 0;
    genesis.block_context.timestamp = 0;
    genesis.block_context.block_hashes = BlockHashes::default();
    storage
        .write(
            Sealed::new_unchecked(genesis, zksync_os_genesis::genesis_header().hash()),
            false,
        )
        .await
        .unwrap();
    storage
        .write(
            Sealed::new_unchecked(alternative_record.clone(), trusted_hash),
            false,
        )
        .await
        .unwrap();

    let state = state_fixture(dir.path()).await;
    let repository = ObserveRepository::default();
    let (applied_sender, applied_receiver) = watch::channel(None);
    let applier = BlockApplier {
        state: state.clone(),
        replay: storage.clone(),
        repositories: repository.clone(),
        config: applier_config(dir.path(), NodeRole::MainNode),
        applied_block_number_sender: applied_sender,
    };
    let (input_sender, input_receiver) = mpsc::channel(1);
    input_sender
        .send(
            canonize(
                BlockPayload {
                    output: alternative_output,
                    record: alternative_record.clone(),
                    command_type: BlockCommandType::Replay,
                    failed_transactions: vec![],
                },
                false,
            )
            .await,
        )
        .await
        .unwrap();
    drop(input_sender);
    let (output_sender, mut output_receiver) = mpsc::channel(1);
    let (reporter, _) = ComponentStateReporter::new("syscoin_archive_applier");
    let error = applier
        .run(
            PeekableReceiver::new(input_receiver),
            output_sender,
            reporter,
        )
        .await
        .expect_err("immutable archived header mismatch must stop the pipeline");
    assert!(
        error
            .to_string()
            .contains("failed to persist replay record for block 1")
    );
    assert!(format!("{error:#}").contains("canonical replay header mismatch"));
    assert!(state.overrides.lock().unwrap().is_empty());
    assert_eq!(*state.inner.block_range_available().end(), 0);
    assert_eq!(repository.0.load(Ordering::Relaxed), 0);
    assert_eq!(*applied_receiver.borrow(), None);
    assert!(output_receiver.recv().await.is_none());
    assert_eq!(storage.get_canonical_block_hash(1), Some(trusted_hash));
    assert_eq!(storage.get_replay_record(1), Some(alternative_record));
}
