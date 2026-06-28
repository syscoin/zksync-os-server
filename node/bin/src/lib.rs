#![feature(allocator_api)]
#![allow(incomplete_features)]
#![feature(generic_const_exprs)]
mod acceptance;
mod batch_sink;
mod batch_work;
pub mod batcher;
mod command_source;
pub mod config;
pub mod default_protocol_version;
// SYSCOIN
mod en_remote_config;
mod init_tx_forwarder;
mod l1_revert;
mod main_node_client;
mod node_state_on_startup;
mod priority_tree_pipeline_step;
pub mod prover_api;
mod prover_block;
mod prover_input_generator;
mod provider;
mod state_initializer;
pub mod tree_manager;
pub mod util;

use crate::batch_sink::{BatchSink, NoOpSink, clear_failing_block_config_task};
use crate::batch_work::{BatchWorkDispatcher, BatchWorkSource, BatchWorkStorage};
use crate::batcher::bitcoin_da_finality_gate::BitcoinDaFinalityGate;
use crate::batcher::bitcoin_da_status_cleanup::BitcoinDaStatusCleanup;
use crate::batcher::bitcoin_da_status_storage::BitcoinDaStatusStorage;
use crate::batcher::{Batcher, BatcherStartupConfig, util::load_genesis_stored_batch_info};
use crate::command_source::{
    ConsensusNodeCommandSource, ExternalNodeCommandSource, RebuildOptions,
};
use crate::config::{
    Config, ProverApiConfig, RebuildConfig, base_token_price_updater_config, gas_adjuster_config,
    report_static_config_metrics,
};
use crate::en_remote_config::load_remote_config;
use crate::init_tx_forwarder::{build_consensus_tx_forwarder, build_static_tx_forwarder};
use crate::l1_revert::revert_l1_on_startup;
use crate::main_node_client::MainNodeClient;
use crate::node_state_on_startup::NodeStateOnStartup;
use crate::prover_api::fake_fri_provers_pool::FakeFriProversPool;
use crate::prover_api::fri_job_manager::FriJobManager;
use crate::prover_api::fri_proving_pipeline_step::FriProvingPipelineStep;
use crate::prover_api::gapless_committer::GaplessCommitter;
use crate::prover_api::gapless_l1_proof_sender::GaplessL1ProofSender;
use crate::prover_api::proof_storage::ProofStorage;
use crate::prover_api::prover_server;
use crate::prover_api::snark_job_manager::{FakeSnarkProver, SnarkJobManager};
use crate::prover_api::snark_proving_pipeline_step::SnarkProvingPipelineStep;
use crate::prover_input_generator::ProverInputGenerator;
use crate::provider::{ProviderKind, build_node_provider};
use crate::state_initializer::StateInitializer;
use crate::tree_manager::TreeManager;
use alloy::consensus::BlobTransactionSidecar;
use alloy::primitives::{Address, BlockHash, BlockNumber};
use alloy::providers::Provider;
use anyhow::Context;
use priority_tree_pipeline_step::PriorityTreePipelineStep;
use reth_tasks::Runtime;
use secrecy::ExposeSecret;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;
use zksync_os_backpressure::{BackpressureMonitor, PipelineTracker};
use zksync_os_base_token_adjuster::{BaseTokenPriceHandle, BaseTokenPriceUpdater};
use zksync_os_batch_verification::{
    BatchVerificationConfig as BatchVerificationPolicyConfig, BatchVerificationPipelineStep,
    BatchVerificationResponder, SyscoinDaVerificationConfig, effective_verification_policy,
};
use zksync_os_contract_interface::ZkChain;
use zksync_os_contract_interface::l1_discovery::{BatchVerificationSL, L1State};
use zksync_os_contract_interface::models::BatchDaInputMode;
use zksync_os_gas_adjuster::GasAdjuster;
use zksync_os_genesis::{FileGenesisInputSource, Genesis, GenesisInputSource};
use zksync_os_internal_config::InternalConfigManager;
use zksync_os_l1_sender::commands::commit::CommitCommand;
use zksync_os_l1_sender::commands::execute::ExecuteCommand;
use zksync_os_l1_sender::commands::prove::ProofCommand;
use zksync_os_l1_sender::pipeline_component::L1Sender;
use zksync_os_l1_sender::upgrade_gatekeeper::UpgradeGatekeeper;
use zksync_os_l1_watcher::L1PersistBatchWatcher;
use zksync_os_l1_watcher::{
    CommittedBatchProvider, L1CommitWatcher, L1ExecuteWatcher, L1FinalizedExecuteWatcher,
    L1RevertWatcher,
};
use zksync_os_mempool::LocalEthCall;
use zksync_os_mempool::Pool;
use zksync_os_mempool::subpools::l2::L2Subpool;
use zksync_os_merkle_tree::{MerkleTree, RocksDBWrapper};
use zksync_os_metadata::NODE_VERSION;
use zksync_os_network::RecordOverride;
use zksync_os_network::VerifyBatch;
use zksync_os_network::protocol::{
    ExternalNodeProtocolConfig, ExternalNodeVerifierConfig, MainNodeProtocolConfig,
    ZksProtocolConfig,
};
use zksync_os_network::service::{NetworkService, PeerVerifyBatch, PeerVerifyBatchResult};
use zksync_os_observability::GENERAL_METRICS;
use zksync_os_pipeline::{Pipeline, PipelineComponent};
use zksync_os_priority_tree::PriorityTreeManager;
use zksync_os_provider::NodeProvider;
use zksync_os_raft::{
    BlockCanonizationEngine, ConsensusRuntimeParts, LeadershipSignal, init_consensus,
    loopback_consensus,
};
use zksync_os_replay_archive::{
    ReplayArchiveGateComponent, ReplayArchiver, ReplayArchivingWriteReplay, init_replay_archive,
};
use zksync_os_reth_compat::provider::ZkProviderFactory;
use zksync_os_revm_consistency_checker::node::RevmConsistencyChecker;
use zksync_os_rpc::{EthCallHandler, RpcStorage};
use zksync_os_sequencer::execution::block_context_provider::{BlockContextProvider, LastBlockSeed};
use zksync_os_sequencer::execution::{BlockApplier, BlockCanonizer, BlockExecutor, FeeProvider};
use zksync_os_status_server::run_status_server;
use zksync_os_storage::db::{BlockReplayStorage, ExecutedBatchStorage};
use zksync_os_storage::in_memory::Finality;
use zksync_os_storage::lazy::RepositoryManager;
use zksync_os_storage_api::{
    FinalityStatus, ReadFinality, ReadReplay, ReadRepository, ReadStateHistory, ReplayRecord,
    WriteReplay, WriteRepository, WriteState,
};
use zksync_os_types::{
    BlockStartCursors, ExecutionVersion, NodeRole, NotAcceptingReason,
    PubdataMode, TransactionAcceptanceState,
};

pub const BLOCK_REPLAY_WAL_DB_NAME: &str = "block_replay_wal";
const RAFT_DB_NAME: &str = "raft";
const STATE_TREE_DB_NAME: &str = "tree";
const PRIORITY_TREE_DB_NAME: &str = "priority_txs_tree";
const REPOSITORY_DB_NAME: &str = "repository";
const BATCH_DB_NAME: &str = "batch";
// SYSCOIN
const BLOCK_APPLIER_OUTPUT_BUFFER_RESERVE: usize = 5;
const REVM_CONSISTENCY_CHECKER_OUTPUT_BUFFER_RESERVE: usize = 5;
const EXECUTION_PIPELINE_IN_FLIGHT_STATE_RESERVE: usize = 4;
const MAX_BATCH_WORK_CHANNEL_CAPACITY: usize = 1024;
pub const INTERNAL_CONFIG_FILE_NAME: &str = "internal_config.json";

// SYSCOIN: `batcher.enabled=false` only means this node does not run local L1 settlement.
// It may produce L2 blocks in consensus HA mode only when explicitly opted in and supplied with
// static fee inputs and compact DA admission credentials; otherwise disabled-batcher main nodes
// stay replay-only/read-only.
fn block_production_enabled(config: &Config) -> bool {
    config.batcher_config.enabled
        || (config.consensus_config.enabled
            && config.sequencer_config.allow_non_batcher_block_production
            && config.fee_config.pubdata_price_override.is_some()
            && compact_edge_da_admission_required(config.l1_sender_config.pubdata_mode)
            && bitcoin_da_rpc_config_complete(config))
}

fn validate_block_production_config(config: &Config, node_role: NodeRole) -> anyhow::Result<()> {
    if !node_role.is_main()
        || config.batcher_config.enabled
        || !config.sequencer_config.allow_non_batcher_block_production
    {
        return Ok(());
    }
    anyhow::ensure!(
        config.consensus_config.enabled,
        "`sequencer.allow_non_batcher_block_production=true` requires `consensus.enabled=true`"
    );
    anyhow::ensure!(
        config.fee_config.pubdata_price_override.is_some(),
        "`sequencer.allow_non_batcher_block_production=true` requires `fee.pubdata_price_override`"
    );
    anyhow::ensure!(
        compact_edge_da_admission_required(config.l1_sender_config.pubdata_mode),
        "`sequencer.allow_non_batcher_block_production=true` requires `l1_sender.pubdata_mode` to use Syscoin blob DA"
    );
    anyhow::ensure!(
        bitcoin_da_rpc_config_complete(config),
        "`sequencer.allow_non_batcher_block_production=true` requires complete Bitcoin DA RPC credentials for compact edge DA admission"
    );
    Ok(())
}

fn compact_edge_da_admission_required(pubdata_mode: Option<PubdataMode>) -> bool {
    matches!(
        pubdata_mode,
        Some(PubdataMode::Blobs | PubdataMode::RelayedL2Calldata)
    )
}

fn edge_da_admission_requested(config: &Config) -> bool {
    let batcher = &config.batcher_config;
    batcher
        .bitcoin_da_rpc_url
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        || batcher
            .bitcoin_da_rpc_user
            .as_ref()
            .map(|secret| secret.expose_secret())
            .is_some_and(|value| !value.trim().is_empty())
        || batcher
            .bitcoin_da_rpc_password
            .as_ref()
            .map(|secret| secret.expose_secret())
            .is_some_and(|value| !value.trim().is_empty())
}

fn syscoin_edge_da_commit_target_required(
    config: &Config,
    node_role: NodeRole,
    effective_pubdata_mode: Option<PubdataMode>,
) -> bool {
    let block_producer_uses_compact_da = node_role.is_main()
        && block_production_enabled(config)
        && edge_da_admission_requested(config)
        && (compact_edge_da_admission_required(effective_pubdata_mode)
            || compact_edge_da_admission_required(config.l1_sender_config.pubdata_mode));

    block_producer_uses_compact_da
        || (config.batch_verification_config.client_enabled && edge_da_admission_requested(config))
}

fn bitcoin_da_rpc_config_complete(config: &Config) -> bool {
    let batcher = &config.batcher_config;
    batcher
        .bitcoin_da_rpc_url
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        && batcher
            .bitcoin_da_rpc_user
            .as_ref()
            .map(|secret| secret.expose_secret())
            .is_some_and(|value| !value.trim().is_empty())
        && batcher
            .bitcoin_da_rpc_password
            .as_ref()
            .map(|secret| secret.expose_secret())
            .is_some_and(|value| !value.trim().is_empty())
}

// SYSCOIN: restarts from an already-persisted non-genesis block must not wait
// for BlockApplier to re-emit that seed block before replay can begin.
fn initial_applied_block_number<T: L2Subpool>(
    block_context_provider: &BlockContextProvider<T>,
) -> Option<BlockNumber> {
    block_context_provider
        .last_block_number()
        .filter(|block_number| *block_number > 0)
}

// SYSCOIN: A read-only main node must reject RPC txs before the sequencer consumes
// its first Produce command, which can be delayed by replay.
fn initial_transaction_acceptance_state(
    node_role: NodeRole,
    max_blocks_to_produce: Option<u64>,
    block_production_enabled: bool,
) -> TransactionAcceptanceState {
    if node_role.is_main() && (!block_production_enabled || max_blocks_to_produce == Some(0)) {
        TransactionAcceptanceState::NotAccepting(vec![NotAcceptingReason::BlockProductionDisabled])
    } else {
        TransactionAcceptanceState::Accepting
    }
}

fn edge_da_admission_config(
    config: &Config,
    commit_tx_target: Address,
) -> anyhow::Result<Option<zksync_os_rpc::EdgeDaAdmissionConfig>> {
    if !compact_edge_da_admission_required(config.l1_sender_config.pubdata_mode) {
        return Ok(None);
    }

    let batcher = &config.batcher_config;
    let rpc_url = batcher
        .bitcoin_da_rpc_url
        .as_deref()
        .filter(|value| !value.trim().is_empty());
    let rpc_user = batcher
        .bitcoin_da_rpc_user
        .as_ref()
        .map(|secret| secret.expose_secret())
        .filter(|value| !value.trim().is_empty());
    let rpc_password = batcher
        .bitcoin_da_rpc_password
        .as_ref()
        .map(|secret| secret.expose_secret())
        .filter(|value| !value.trim().is_empty());
    if !edge_da_admission_requested(config) {
        return Ok(None);
    }

    Ok(Some(zksync_os_rpc::EdgeDaAdmissionConfig {
        commit_tx_target,
        rpc_url: rpc_url
            .context(
                "`batcher.bitcoin_da_rpc_url` must be set when edge DA admission is configured",
            )?
            .to_owned(),
        rpc_user: rpc_user
            .context(
                "`batcher.bitcoin_da_rpc_user` must be set when edge DA admission is configured",
            )?
            .to_owned(),
        rpc_password: rpc_password
            .context(
                "`batcher.bitcoin_da_rpc_password` must be set when edge DA admission is configured",
            )?
            .to_owned(),
        poda_url: batcher.bitcoin_da_poda_url.clone(),
        wallet_name: batcher.bitcoin_da_wallet_name.clone(),
        request_timeout: batcher.bitcoin_da_request_timeout,
    }))
}

// SYSCOIN: batch-verifier clients use the same Bitcoin DA settings as the batcher
// to independently check committed blob availability before returning signatures.
fn syscoin_da_verification_config(config: &Config) -> Option<SyscoinDaVerificationConfig> {
    let batcher = &config.batcher_config;
    let rpc_url = batcher
        .bitcoin_da_rpc_url
        .as_ref()
        .filter(|value| !value.trim().is_empty())?
        .to_owned();
    let rpc_user = batcher.bitcoin_da_rpc_user.clone()?;
    let rpc_password = batcher.bitcoin_da_rpc_password.clone()?;

    Some(SyscoinDaVerificationConfig {
        rpc_url,
        rpc_user,
        rpc_password,
        poda_url: batcher.bitcoin_da_poda_url.clone(),
        wallet_name: batcher.bitcoin_da_wallet_name.clone(),
        request_timeout: batcher.bitcoin_da_request_timeout,
    })
}

#[allow(clippy::too_many_arguments)]
pub async fn run<State: ReadStateHistory + WriteState + StateInitializer + Clone>(
    runtime: &Runtime,
    config: Config,
) {
    report_static_config_metrics(&config);

    let node_role = config.general_config.node_role;
    let role: &'static str = node_role.as_str();

    let process_started_at = Instant::now();
    GENERAL_METRICS.process_started_at[&(NODE_VERSION, role)].set(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
    );
    // SYSCOIN: `batcher.enabled=false` skips L1 settlement entirely, so disabled-batcher
    // main nodes must be able to start without enabling L1 senders.
    if config.batcher_config.enabled && !config.l1_sender_config.enabled {
        unimplemented!("running without L1 Senders is temporarily not supported");
    }
    validate_block_production_config(&config, node_role)
        .expect("invalid block production configuration");
    tracing::info!(version = NODE_VERSION, role, "Initializing Node");

    // One client for all main-node RPC; `None` on a main node (nothing to talk to).
    let main_node_client = config
        .general_config
        .main_node_rpc_url
        .as_ref()
        .map(|url| MainNodeClient::new(url).expect("failed to build main node RPC client"));

    let (bridgehub_address, bytecode_supplier_address, chain_id, genesis_input_source) =
        if node_role.is_main() {
            let genesis_input_source: Arc<dyn GenesisInputSource> =
                Arc::new(FileGenesisInputSource::new(
                    config
                        .genesis_config
                        .genesis_input_path
                        .clone()
                        .expect("Missing `genesis_input_path`"),
                ));
            (
                config
                    .genesis_config
                    .bridgehub_address
                    .expect("Missing `bridgehub_address`"),
                config
                    .genesis_config
                    .bytecode_supplier_address
                    .expect("Missing `bytecode_supplier_address`"),
                config.genesis_config.chain_id.expect("Missing `chain_id`"),
                genesis_input_source,
            )
        } else {
            let client = main_node_client
                .as_ref()
                .expect("Missing `main_node_rpc_url` in external node config");
            load_remote_config(client, &config.genesis_config)
                .await
                .expect("Cannot load remote config from Main Node")
        };

    // This is the only place where we initialize L1 provider, every component shares the same
    // cloned provider.
    let l1_provider = build_node_provider(
        &config.l1_provider_config,
        config.l1_watcher_config.poll_interval,
        config.l1_watcher_config.finalized_poll_interval,
        config.l1_watcher_config.logs_cache_capacity,
        ProviderKind::L1,
    )
    .await;
    // SYSCOIN: Optional archive-capable L1 provider for startup historical batch reads only.
    let l1_archive_provider = if let Some(archive_config) = &config.l1_archive_provider_config {
        Some(
            build_node_provider(
                archive_config,
                config.l1_watcher_config.poll_interval,
                config.l1_watcher_config.finalized_poll_interval,
                config.l1_watcher_config.logs_cache_capacity,
                ProviderKind::L1Archive,
            )
            .await,
        )
    } else {
        None
    };
    let gateway_provider = if let Some(gw_config) = &config.gateway_provider_config {
        Some(
            build_node_provider(
                gw_config,
                config.l1_watcher_config.poll_interval,
                config.l1_watcher_config.finalized_poll_interval,
                config.l1_watcher_config.logs_cache_capacity,
                ProviderKind::Gateway,
            )
            .await,
        )
    } else {
        None
    };

    // Genesis and the repository manager are initialized here (before the startup revert) so
    // that the `from_block_hash` guard can read the current local block hash.
    let diamond_proxy_l1 =
        L1State::resolve_diamond_proxy_l1(l1_provider.clone(), bridgehub_address, chain_id)
            .await
            .expect("failed to resolve L1 diamond proxy");

    let genesis = Genesis::new(
        genesis_input_source.clone(),
        diamond_proxy_l1.clone(),
        chain_id,
    );

    tracing::info!("Initializing RepositoryManager");
    let repositories = RepositoryManager::new(
        config.general_config.blocks_to_retain_in_memory,
        config.general_config.rocks_db_path.join(REPOSITORY_DB_NAME),
        &genesis,
    )
    .await;

    tracing::info!("Initializing BlockReplayStorage");
    let (block_replay_storage, inserted_genesis_replay_record) = BlockReplayStorage::new(
        &config
            .general_config
            .rocks_db_path
            .join(BLOCK_REPLAY_WAL_DB_NAME),
        &genesis,
    )
    .await;

    // Apply the `from_block_hash` idempotency guard once, up front, to derive the *effective*
    // rebuild config used for the rest of startup. If the configured rebuild already ran on a
    // prior startup (hash no longer matches), it is dropped here so BOTH the L1 revert (stage 1,
    // below) and the local block rebuild (stage 2, downstream) are skipped.
    let rebuild_config = if node_role.is_main() {
        let configured = config.sequencer_config.rebuild.clone();
        match configured.as_ref().and_then(|r| r.bounds()) {
            Some(bounds)
                if !from_block_hash_matches(
                    &repositories,
                    &block_replay_storage,
                    bounds.from_block_number,
                    bounds.from_block_hash,
                )
                .expect("failed to evaluate startup rebuild from_block_hash guard") =>
            {
                None
            }
            _ => configured,
        }
    } else {
        config.sequencer_config.rebuild.clone()
    };
    // What the local block-rebuild stage (stage 2) should replay, if anything.
    let rebuild_options = rebuild_config.as_ref().and_then(|r| r.rebuild_options());

    // Fetch the L1 state, performing the configured startup L1 revert first.
    tracing::info!("Reading L1 state");
    let l1_state = fetch_l1_state_with_startup_revert(
        &config,
        node_role,
        rebuild_config.as_ref(),
        &l1_provider,
        gateway_provider.as_ref(),
        bridgehub_address,
        chain_id,
    )
    .await
    .expect("failed to determine L1 state");

    let settles_on_gateway = l1_state.settles_on_gateway();
    let sl_provider = if l1_state.l1_chain_id == l1_state.sl_chain_id {
        l1_provider.clone()
    } else {
        gateway_provider.clone().unwrap()
    };
    // SYSCOIN: Settlement mode is discovered from the L1 diamond, not from the
    // optional Gateway provider config. A Gateway RPC may be configured before
    // or after migration while the chain still settles directly on L1.
    //
    // SYSCOIN: Startup cursor resolution can require historical L1 state. Keep
    // live watchers on the configured live providers, but use the archive L1
    // provider for startup-only L1 binary searches when available.
    let archive_lookup_diamond_proxy_l1 = l1_archive_provider
        .as_ref()
        .map(|provider| ZkChain::new(*l1_state.diamond_proxy_l1.address(), provider.clone()));
    let archive_lookup_diamond_proxy_sl = if settles_on_gateway {
        None
    } else {
        archive_lookup_diamond_proxy_l1.clone()
    };
    tracing::info!(?l1_state, settles_on_gateway, "L1 state");
    l1_state.report_metrics();
    if node_role.is_main() {
        // SYSCOIN
        validate_batch_verification_startup_policy(
            &config.batch_verification_config,
            &l1_state.batch_verification,
        );
        check_batch_verification_mismatch(
            &config.batch_verification_config,
            &l1_state.batch_verification,
        );
        if config.batcher_config.enabled {
            check_required_operator_keys(&config, settles_on_gateway);
        }
    }

    // Effective pubdata mode used by all block-producing components: read from config only when
    // the chain settles on L1. When settling on Gateway, it is derived from the gateway's DA
    // input mode: Rollup gateway -> RelayedL2Calldata, Validium gateway -> Validium.
    let effective_pubdata_mode: Option<PubdataMode> =
        if node_role.is_main() && config.batcher_config.enabled {
            Some(effective_main_node_pubdata_mode(
                &config,
                settles_on_gateway,
                l1_state.da_input_mode,
            ))
        } else {
            // External and replay-only main nodes do not produce blocks; pubdata mode is irrelevant.
            None
        };
    if let (Some(pubdata_mode), true) = (effective_pubdata_mode, node_role.is_main()) {
        match (pubdata_mode, l1_state.da_input_mode) {
            (
                PubdataMode::Calldata | PubdataMode::Blobs | PubdataMode::RelayedL2Calldata,
                BatchDaInputMode::Validium,
            )
            | (PubdataMode::Validium, BatchDaInputMode::Rollup) => {
                panic!(
                    "Pubdata mode doesn't correspond to pricing mode from the l1. \
                    L1 mode: {:?}, effective pubdata mode: {:?}",
                    l1_state.da_input_mode, pubdata_mode
                );
            }
            _ => {}
        }
    }
    prepare_raft_storage(&config).expect("failed to prepare raft storage");

    tracing::info!("Initializing Tree RocksDB");
    let tree_db = TreeManager::load_or_initialize_tree(
        Path::new(&config.general_config.rocks_db_path.join(STATE_TREE_DB_NAME)),
        &genesis,
    )
    .await;

    let (genesis_root_hash, genesis_root_leaves) = tree_db
        .root_info(0)
        .expect("Failed to get genesis root info")
        .expect("tree is not initialized");
    let tree_for_rpc = Arc::new(tree_db.clone());
    // SYSCOIN: Reuse executed batch storage to hydrate committed batches before
    // falling back to historical archive L1 reads during startup.
    let persistent_batch_storage =
        ExecutedBatchStorage::new(&config.general_config.rocks_db_path.join(BATCH_DB_NAME));

    let committed_batch_provider = CommittedBatchProvider::new(
        runtime,
        &l1_state,
        config.l1_watcher_config.max_blocks_to_process,
        persistent_batch_storage.clone(),
        l1_archive_provider.clone(),
        || async {
            let genesis_state = genesis.state().await;
            load_genesis_stored_batch_info(genesis_state, genesis_root_hash, genesis_root_leaves)
                .await
                .unwrap()
        },
    )
    .await
    .expect("failed to init CommittedBatchProvider");

    let state = State::new(&config.general_config, &genesis).await;

    tracing::info!("Initializing mempools");
    let zk_provider_factory = ZkProviderFactory::new(state.clone(), repositories.clone(), chain_id);
    let l2_subpool = zksync_os_mempool::subpools::l2::in_memory(
        zk_provider_factory.clone(),
        config.mempool_config.clone().into(),
        config.tx_validator_config.clone().into(),
    );

    let (
        last_l1_committed_block,
        last_l1_proved_block,
        last_l1_executed_block,
        last_l1_finalized_executed_block,
    ) = commit_proof_execute_block_numbers(&l1_state, &committed_batch_provider).await;

    let node_startup_state = NodeStateOnStartup {
        node_role,
        l1_state: l1_state.clone(),
        state_block_range_available: state.block_range_available(),
        block_replay_storage_last_block: block_replay_storage.latest_record(),
        tree_last_block: tree_db
            .latest_version()
            .expect("cannot read tree last processed block after initialization")
            .expect("tree database is not initialized"),
        repositories_persisted_block: repositories.get_latest_block(),
        last_l1_committed_block,
        last_l1_proved_block,
        last_l1_executed_block,
    };

    if let Some(from_block_number) = rebuild_options.as_ref().map(|o| o.from_block_number)
        && node_role.is_main()
    {
        // The assertion is only relevant for the main node.
        // External node can be started at any point and doesn't have to be in sync with L1.
        // But the main node is expected to only produce blocks on top of committed L1 blocks,
        // as those can't be re-sequenced.
        assert!(
            from_block_number > node_startup_state.last_l1_committed_block,
            "rebuild_from_block_number must be > last_l1_committed_block, got {} <= {}",
            from_block_number,
            node_startup_state.last_l1_committed_block
        );
    }

    let finality_storage = Finality::new(FinalityStatus {
        last_committed_block: last_l1_committed_block,
        last_committed_batch: l1_state.last_committed_batch,
        last_executed_block: last_l1_executed_block,
        last_executed_batch: l1_state.last_executed_batch,
        last_finalized_executed_block: last_l1_finalized_executed_block,
        last_finalized_executed_batch: l1_state.last_finalized_executed_batch,
    });

    // `starting_block` - the first block to go through the pipeline. Invariant: a replay record for
    // this block must already exist. Note that this holds for `starting_block=0` as genesis is
    // always present in the system.
    let starting_block = if node_startup_state.l1_state.last_committed_batch > 0 {
        // todo: ideally this should be searched through p2p networking instead of RPC
        //       but too many things depend on this being initialized here right now
        //       once refactored we can get rid of `main_node_rpc_url` config param
        let last_matching_block = if let Some(client) = &main_node_client {
            find_last_matching_main_node_block(&repositories, client)
                .await
                .expect("Failed to find last matching block with main node")
        } else {
            node_startup_state.repositories_persisted_block
        };
        // Some batches committed - starting from an already committed batch
        determine_starting_block(
            &config,
            &node_startup_state,
            &state,
            last_matching_block,
            rebuild_options.as_ref(),
        )
    } else {
        // No batches committed - starting from genesis.
        0
    };

    tracing::info!(
        config.general_config.min_blocks_to_replay,
        config.general_config.force_starting_block_number,
        ?node_startup_state,
        starting_block,
        blocks_to_replay = node_startup_state.block_replay_storage_last_block + 1 - starting_block,
        "Node state on startup"
    );

    node_startup_state.assert_consistency();

    // MN sends `VerifyBatch` requests to the network and receives `PeerVerifyBatchResult`s back.
    let (verify_request_tx, verify_request_rx) = tokio::sync::mpsc::channel::<VerifyBatch>(16);
    let (verify_result_tx, verify_result_rx) =
        tokio::sync::mpsc::channel::<PeerVerifyBatchResult>(128);
    // `replay_*` carries replay records from the network service into the EN pipeline.
    let (replay_sender, replays_for_sequencer) = tokio::sync::mpsc::channel(128);
    // EN receives peer verification requests and broadcasts signed responses back to the network.
    let (verify_batch_tx, verify_batch_rx) = tokio::sync::mpsc::channel::<PeerVerifyBatch>(128);
    let (outgoing_verify_results, _) =
        tokio::sync::broadcast::channel::<PeerVerifyBatchResult>(128);

    let ConsensusRuntimeParts {
        canonization_engine,
        leadership,
        raft,
    } = if config.consensus_config.enabled {
        init_consensus(
            runtime,
            config
                .consensus_config
                .clone()
                .into_raft_consensus_config(
                    &config.network_config,
                    config.general_config.rocks_db_path.join(RAFT_DB_NAME),
                )
                .expect("failed to build raft consensus config"),
            Box::new(block_replay_storage.clone()),
        )
        .await
        .expect("failed to initialize consensus engine")
    } else {
        tracing::info!("openraft consensus is disabled - assuming perpetual leader role");
        loopback_consensus()
    };
    let (raft_protocol_handler, raft_bootstrapper, raft_status_rx) = match raft {
        Some(raft) => (
            Some(raft.protocol_handler),
            raft.bootstrapper,
            Some(raft.status_rx),
        ),
        None => (None, None, None),
    };
    if config.network_config.enabled {
        tracing::info!("initializing p2p networking");
        let batch_verification_policy_config: BatchVerificationPolicyConfig =
            config.batch_verification_config.clone().into();
        let network_service = if node_role.is_main() {
            let (_, accepted_verifier_signers) =
                effective_verification_policy(&batch_verification_policy_config, &l1_state);
            NetworkService::new(
                config.network_config.clone().into(),
                runtime.clone(),
                ZksProtocolConfig::MainNode(MainNodeProtocolConfig {
                    accepted_verifier_signers,
                    verify_result_tx: verify_result_tx.clone(),
                }),
                block_replay_storage.clone(),
                zk_provider_factory,
                raft_protocol_handler,
            )
            .await
        } else {
            // SYSCOIN: EN replay records are accepted only from configured main-node boot peers.
            let trusted_main_node_peers = config
                .network_config
                .boot_nodes
                .iter()
                .map(|peer| peer.id)
                .collect();
            let record_overrides = config
                .sequencer_config
                .en_replay_record_overrides
                .iter()
                .map(|(block_number, db_key)| RecordOverride {
                    block_number: *block_number,
                    db_key: db_key.clone(),
                })
                .collect();
            NetworkService::new(
                config.network_config.clone().into(),
                runtime.clone(),
                ZksProtocolConfig::ExternalNode(ExternalNodeProtocolConfig {
                    starting_block: Arc::new(RwLock::new(starting_block)),
                    record_overrides,
                    max_blocks_per_message: config
                        .sequencer_config
                        .en_max_blocks_per_replay_message,
                    trusted_main_node_peers,
                    replay_sender,
                    verification: config.batch_verification_config.client_enabled.then(|| {
                        ExternalNodeVerifierConfig {
                            signing_key: config.batch_verification_config.signing_key.clone(),
                            verify_batch_tx: verify_batch_tx.clone(),
                            outgoing_verify_results: outgoing_verify_results.clone(),
                        }
                    }),
                }),
                block_replay_storage.clone(),
                zk_provider_factory,
                raft_protocol_handler,
            )
            .await
        }
        .expect("failed to create network service");
        network_service.spawn(runtime, node_role.is_main().then_some(verify_request_rx));
        if let Some(bootstrapper) = raft_bootstrapper {
            bootstrapper
                .bootstrap_if_needed()
                .await
                .expect("failed to run raft bootstrap process");
        }
    } else if node_role.is_main() {
        tracing::info!(
            "p2p networking is disabled; to enable set `network.enabled=true` and populate `network.secret_key`"
        );
    } else {
        panic!(
            "EN cannot run without p2p networking; to fix: \
            set `network.enabled=true` to enable p2p networking, \
            populate `network.secret_key` with a 256-bit ECDSA key (can be randomly generated locally), \
            populate `network.boot_nodes` with at least one known node from the chain. \
            See https://github.com/matter-labs/zksync-os-server/pull/873 for full rollout instructions."
        );
    }

    // Channel from L1Sender<CommitCommand> to L1CommitWatcher.
    // Initialized to startup's last_committed_batch so any commit above that value
    // which the pipeline didn't submit in this session triggers a restart.
    let (commit_submitted_tx, commit_submitted_rx) =
        watch::channel(node_startup_state.l1_state.last_committed_batch);

    tracing::info!("Initializing L1 Watchers");
    runtime.spawn_critical_task(
        "l1 commit watcher",
        L1CommitWatcher::create_watcher(
            config.l1_watcher_config.clone().into(),
            node_startup_state.l1_state.diamond_proxy_sl.clone(),
            archive_lookup_diamond_proxy_sl.clone(),
            committed_batch_provider.clone(),
            finality_storage.clone(),
            l1_state.sl_block_number,
            // SYSCOIN: this watcher follows the active settlement layer, so validate
            // against the SL provider chain ID and preserve the configured confirmations.
            node_startup_state.l1_state.sl_chain_id,
            // Only nodes that actually submit commit txs locally should arm the
            // `UnexpectedCommit` guard — otherwise consensus followers configured with
            // `batcher_config.enabled = false` panic the moment the leader's commit lands on L1.
            (node_role.is_main() && config.batcher_config.enabled).then_some(commit_submitted_rx),
        )
        .await
        .expect("failed to start L1 commit watcher")
        .run(()),
    );

    runtime.spawn_critical_task(
        "l1 execute watcher",
        L1ExecuteWatcher::create_watcher(
            config.l1_watcher_config.clone().into(),
            node_startup_state.l1_state.diamond_proxy_sl.clone(),
            archive_lookup_diamond_proxy_sl.clone(),
            committed_batch_provider.clone(),
            finality_storage.clone(),
            // SYSCOIN: this watcher follows the active settlement layer, so validate
            // against the SL provider chain ID and preserve the configured confirmations.
            node_startup_state.l1_state.sl_chain_id,
        )
        .await
        .expect("failed to start L1 execute watcher")
        .run(()),
    );

    runtime.spawn_critical_task(
        "l1 finalized execute watcher",
        L1FinalizedExecuteWatcher::create_finalized_watcher(
            config.l1_watcher_config.clone().into(),
            node_startup_state.l1_state.diamond_proxy_sl.clone(),
            archive_lookup_diamond_proxy_sl.clone(),
            committed_batch_provider.clone(),
            finality_storage.clone(),
        )
        .await
        .expect("failed to start finalized L1 execute watcher")
        .run(()),
    );

    // External nodes restart themselves on an L1 batch revert to resync correct data.
    if node_role.is_external() {
        runtime.spawn_critical_task(
            "l1 revert watcher",
            L1RevertWatcher::create_watcher(
                config.l1_watcher_config.clone().into(),
                node_startup_state.l1_state.diamond_proxy_sl.clone(),
                node_startup_state.l1_state.sl_block_number,
            )
            .run(),
        );
    }

    let first_replay_record = block_replay_storage.get_replay_record(starting_block);
    assert!(
        first_replay_record.is_some() || starting_block == 1,
        "Unless it's a new chain, replay record must exist"
    );

    let next_cursors = first_replay_record
        .as_ref()
        .map_or(BlockStartCursors::default(), |record| {
            record.starting_cursors.clone()
        });
    let last_block_seed = (starting_block > 0).then(|| {
        let previous_block = starting_block - 1;
        // SYSCOIN: seed replay validation from local canonical replay storage so the first
        // non-genesis replay record cannot choose its own newest previous-block hash.
        let record = block_replay_storage
            .get_replay_record(previous_block)
            .unwrap_or_else(|| panic!("missing replay record for seed block {previous_block}"));
        let hash = block_replay_storage
            .get_canonical_block_hash(previous_block)
            .unwrap_or_else(|| {
                panic!("missing canonical block hash for seed block {previous_block}")
            });
        LastBlockSeed {
            record,
            hash,
            next_cursors: next_cursors.clone(),
        }
    });

    let current_protocol_version = if let Some(record) = &first_replay_record {
        &record.protocol_version
    } else {
        &genesis.genesis_upgrade_tx().await.protocol_version
    };
    let syscoin_edge_da_commit_target = resolve_syscoin_edge_da_commit_target(
        &l1_state,
        settles_on_gateway,
        syscoin_edge_da_commit_target_required(&config, node_role, effective_pubdata_mode),
    );

    if config
        .sequencer_config
        .tx_validator
        .deployment_filter
        .enabled
    {
        let exec_version = ExecutionVersion::try_from(current_protocol_version)
            .expect("Cannot determine execution version");
        assert!(
            exec_version >= ExecutionVersion::V6,
            "Deployment filter requires execution version V6 or later (protocol >= v31.0), \
             but current protocol version {current_protocol_version} uses {exec_version:?}"
        );
    }

    if config
        .sequencer_config
        .tx_validator
        .policy_service
        .url
        .is_some()
    {
        let exec_version = ExecutionVersion::try_from(current_protocol_version)
            .expect("Cannot determine execution version");
        assert!(
            exec_version >= ExecutionVersion::V6,
            "Policy service requires execution version V6 or later (protocol >= v31.0), \
             but current protocol version {current_protocol_version} uses {exec_version:?}"
        );
    }

    // Transaction acceptance state - tracks whether we're accepting new transactions
    // Main nodes: accepts, but may switch to reject when `sequencer_max_blocks_to_produce` blocks are produced
    // External nodes: always accepts, but may be rejected on the main node side during forwarding
    let block_production_enabled = block_production_enabled(&config);
    let (tx_acceptance_state_sender, tx_acceptance_state_receiver) =
        watch::channel(initial_transaction_acceptance_state(
            node_role,
            config.sequencer_config.max_blocks_to_produce,
            block_production_enabled,
        ));

    let (stop_sender, stop_receiver) = watch::channel(false);
    let stop_sender_for_shutdown = stop_sender.clone();
    runtime.spawn_with_graceful_shutdown_signal(|shutdown| async move {
        let _guard = shutdown.await;
        let _ = stop_sender_for_shutdown.send(true);
    });

    let tx_forwarder = if let Some(url) = config.general_config.main_node_rpc_url.as_ref() {
        Some(build_static_tx_forwarder(url).await)
    } else if config.consensus_config.enabled {
        let status_rx = raft_status_rx
            .clone()
            .expect("consensus status receiver must be present when consensus is enabled");
        Some(build_consensus_tx_forwarder(&config, status_rx).await)
    } else {
        None
    };

    let (last_constructed_block_ctx_sender, last_constructed_block_ctx_receiver) =
        watch::channel(None);

    tracing::info!("Initializing pubdata price provider");
    // Channels for GasAdjuster->BlockContextProvider communication.
    let (pubdata_price_sender, pubdata_price_receiver) = watch::channel(None);
    let (blob_fill_ratio_sender, blob_fill_ratio_receiver) = watch::channel(None);
    // Channel for Batcher->GasAdjuster communication. Batcher send sidecar to gas adjuster to estimate blob fill ratio.
    let (sidecar_sender, sidecar_receiver) = tokio::sync::mpsc::channel(10);
    if node_role.is_main() && config.batcher_config.enabled {
        let pubdata_mode = effective_pubdata_mode
            .expect("effective pubdata mode must be set when the Main Node batcher is enabled");
        let max_priority_fee_per_gas = if settles_on_gateway {
            config.gateway_sender_config.max_priority_fee_per_gas.0
        } else {
            config.l1_sender_config.max_priority_fee_per_gas.0
        };
        let gas_adjuster_config = gas_adjuster_config(
            config.gas_adjuster_config.clone(),
            pubdata_mode,
            max_priority_fee_per_gas,
            &config.batcher_config,
        );
        let gas_adjuster = GasAdjuster::new(
            sl_provider.clone().erased(),
            gas_adjuster_config,
            pubdata_price_sender,
            blob_fill_ratio_sender,
            sidecar_receiver,
        )
        .await
        .unwrap();
        runtime.spawn_critical_task("gas adjuster", gas_adjuster.run());
    }

    // ========== Start BlockContextProvider and its state ===========
    tracing::info!("Initializing BlockContextProvider");

    // The base token price updater owns the price channel and hands back a cloneable handle.
    // External nodes don't run the updater, so they get a `pending` handle whose value stays unset.
    let base_token_price_handle = if node_role.is_main() {
        let external_price_api_client_config = config
            .external_price_api_client_config
            .clone()
            .expect("external_price_api_client config must be set for Main Node");
        let (base_token_price_updater, base_token_price_handle) = BaseTokenPriceUpdater::new(
            &l1_state,
            base_token_price_updater_config(
                &config.base_token_price_updater_config,
                &config.l1_sender_config,
            ),
            external_price_api_client_config.into(),
        )
        .await
        .expect("Failed to initialize BaseTokenPriceUpdater");
        runtime.spawn_critical_task("base token price updater", base_token_price_updater.run());
        base_token_price_handle
    } else {
        BaseTokenPriceHandle::pending()
    };

    // todo: `BlockContextProvider` initialization and its dependencies
    // should be moved to `sequencer`
    let fee_provider = FeeProvider::new(
        config.fee_config.clone().into(),
        pubdata_price_receiver,
        blob_fill_ratio_receiver,
        base_token_price_handle.clone(),
        effective_pubdata_mode,
    );

    let rpc_storage = RpcStorage::new(
        repositories.clone(),
        block_replay_storage.clone(),
        finality_storage.clone(),
        persistent_batch_storage.clone(),
        state.clone(),
        tree_for_rpc,
    );

    // Mini-component capable of doing local `eth_call` without going through RPC. Needed for
    // interop fee updater so it can query the current interop fee.
    let local_eth_call = Box::new(EthCallHandler::new(
        config.rpc_config.clone().into(),
        rpc_storage.clone(),
        chain_id,
        last_constructed_block_ctx_receiver.clone(),
        // Interop fee updater runs inside the node and is not a user-facing
        // RPC surface, so the admit boundary doesn't apply.
        None,
    )) as Box<dyn LocalEthCall>;

    let pool = Pool::new(
        runtime.clone(),
        genesis.clone(),
        &node_startup_state.l1_state,
        zksync_os_mempool::Config {
            node_role,
            chain_id,
            gateway_chain_id: config.general_config.gateway_chain_id,
            interop_roots_per_tx: config.sequencer_config.interop_roots_per_tx,
            bytecode_supplier_address,
            // SYSCOIN: use the archive-capable L1 lookup chain for startup cursor resolution.
            archive_lookup_diamond_proxy_l1: archive_lookup_diamond_proxy_l1.clone(),
            l1_watcher_config: config.l1_watcher_config.clone().into(),
            interop_fee_updater_config: config.interop_fee_updater_config.clone().into(),
        },
        local_eth_call,
        base_token_price_handle.clone(),
        // todo: eventually this should be initialized inside `Pool::new`
        l2_subpool.clone(),
    )
    .await
    .expect("failed to create mempool");
    let block_context_provider = BlockContextProvider::new(
        fee_provider,
        pool,
        zksync_os_sequencer::execution::block_context_provider::Config {
            l2_chain_id: chain_id,
            l1_chain_id: node_startup_state.l1_state.l1_chain_id,
            gas_limit: config.sequencer_config.block_gas_limit,
            pubdata_limit: config.sequencer_config.block_pubdata_limit_bytes,
            fee_collector_address: config.sequencer_config.fee_collector_address,
            block_time: config.sequencer_config.block_time,
            service_block_delay: config.sequencer_config.service_block_delay,
            max_transactions_in_block: config.sequencer_config.max_transactions_in_block,
            // We set the value to the same as for the batch, since it should be enforced by batcher, but don't want to exceed it for the block
            interop_roots_per_block: config.batcher_config.interop_roots_per_batch_limit,
        },
        &node_startup_state.l1_state.settlement_layer_intervals,
        last_constructed_block_ctx_sender,
        last_block_seed,
    );

    // ========== Start L1 Persist Batch Watcher ===========

    // SYSCOIN: persist watcher setup may lazily resolve historical Gateway intervals that still
    // need persistence.
    runtime.spawn_critical_task("l1 batch persist watcher", {
        let config = config.l1_watcher_config.clone();
        let settlement_layer_intervals = node_startup_state
            .l1_state
            .settlement_layer_intervals
            .clone();
        let persistent_batch_storage = persistent_batch_storage.clone();
        // SYSCOIN: archive provider for startup-only historical cursor lookups.
        let archive_l1_provider = l1_archive_provider.clone();
        async move {
            L1PersistBatchWatcher::create_watcher(
                config.into(),
                settlement_layer_intervals,
                persistent_batch_storage,
                archive_l1_provider,
            )
            .run(())
            .await
        }
    });

    // ========== Start Sequencer ===========
    let repositories_clone = repositories.clone();
    runtime.spawn_critical_task(
        "repository persist loop",
        repositories_clone.run_persist_loop(),
    );
    let state_clone = state.clone();
    runtime.spawn_critical_task(
        "state compact loop",
        state_clone.compact_periodically_optional(),
    );

    let replay_archive =
        init_replay_archive(config.replay_archive_config.clone().into(), runtime).await;
    if let (Some((replay_archive_sender, _)), Some(inserted_genesis_replay_record)) =
        (&replay_archive, inserted_genesis_replay_record)
    {
        let (genesis_replay_record, genesis_hash) = inserted_genesis_replay_record.split();
        replay_archive_sender
            .send((genesis_hash, genesis_replay_record))
            .await
            .expect("replay archive component stopped before accepting genesis replay record");
    }
    let (replay_archive_sender, replay_archiver) = replay_archive.unzip();
    let archiving_block_replay_storage =
        ReplayArchivingWriteReplay::new(block_replay_storage, replay_archive_sender);

    let backpressure_acceptance_rx = if node_role.is_main() {
        run_main_node_pipeline(
            &config,
            sl_provider.clone(),
            node_startup_state,
            archiving_block_replay_storage,
            runtime,
            state.clone(),
            starting_block,
            rebuild_options,
            repositories.clone(),
            block_context_provider,
            tree_db,
            finality_storage.clone(),
            chain_id,
            tx_acceptance_state_sender,
            sidecar_sender,
            committed_batch_provider.clone(),
            canonization_engine,
            leadership,
            stop_receiver.clone(),
            commit_submitted_tx,
            verify_request_tx,
            verify_result_rx,
            settles_on_gateway,
            syscoin_edge_da_commit_target,
            effective_pubdata_mode,
            replay_archiver,
        )
        .await
    } else {
        run_en_pipeline(
            &config,
            replays_for_sequencer,
            committed_batch_provider.clone(),
            node_startup_state,
            archiving_block_replay_storage,
            runtime,
            block_context_provider,
            state.clone(),
            tree_db,
            repositories.clone(),
            finality_storage.clone(),
            stop_receiver.clone(),
            tx_acceptance_state_sender,
            chain_id,
            syscoin_edge_da_commit_target,
            verify_batch_rx,
            outgoing_verify_results.clone(),
        )
        .await
    };

    // Aggregate all "not accepting" signals into a single combined receiver for the RPC server.
    // Register additional sources here as needed — no other logic changes required.
    let combined_acceptance_rx = {
        let (mut gate, rx) = acceptance::TxAcceptanceGate::new();
        gate.register(tx_acceptance_state_receiver); // BlockProductionDisabled
        gate.register(backpressure_acceptance_rx); // PipelineBackpressure
        runtime.spawn_critical_task("tx acceptance gate", gate.run(stop_receiver.clone()));
        rx
    };

    // ======== Start Status Server ========
    if config.status_server_config.enabled {
        let addr: SocketAddr = config
            .status_server_config
            .address
            .parse()
            .expect("malformed `status_server.address`");
        runtime.spawn_critical_with_graceful_shutdown_signal(
            "status server",
            |shutdown| async move {
                run_status_server(addr, shutdown, raft_status_rx)
                    .await
                    .expect("failed to run status server");
            },
        );
    }

    // =========== Start JSON RPC ========
    let repositories_for_wait = repositories.clone();
    let wait_for_db = async move {
        // Wait for repositories to be ready to be used in RPC.
        repositories_for_wait
            .wait_for_db_ready_to_process_blocks()
            .await;
    };
    let mut rpc_config: zksync_os_rpc::RpcConfig = config.rpc_config.clone().into();
    // SYSCOIN: Gateway must reject child-chain compact DA commit txs before block inclusion
    // if the referenced Bitcoin DA hashes are not retrievable yet.
    rpc_config.edge_da_admission = edge_da_admission_config(&config, syscoin_edge_da_commit_target)
        .expect("failed to build edge DA admission config");
    let rpc_policy_client = config
        .sequencer_config
        .tx_validator
        .policy_service
        .build_client(zksync_os_tx_validators::policy_client::Component::Rpc);
    zksync_os_rpc::spawn(
        rpc_config,
        chain_id,
        bridgehub_address,
        bytecode_supplier_address,
        rpc_storage,
        l2_subpool,
        genesis_input_source,
        combined_acceptance_rx,
        last_constructed_block_ctx_receiver,
        tx_forwarder,
        gateway_provider.map(|p| p.erased()),
        rpc_policy_client,
        runtime,
        wait_for_db,
    )
    .await
    .expect("failed to spawn rpc server");
    let startup_time = process_started_at.elapsed();
    GENERAL_METRICS.startup_time[&"total"].set(startup_time.as_secs_f64());
    tracing::info!("All components scheduled for initialization in {startup_time:?}");
}

/// Checks whether block `rebuild.from_block_number` currently has the expected `rebuild.from_block_hash`.
///
/// Returns `Ok(false)` (operation should be skipped) when the hashes differ — distinguishing the
/// two reasons in the logs:
/// - block missing locally: likely a misconfigured `from_block_number` (typo / beyond local tip);
/// - hash changed: the rebuild/revert already ran on a previous startup (the expected case).
fn from_block_hash_matches(
    repositories: &dyn ReadRepository,
    replay_storage: &dyn ReadReplay,
    from_block_number: u64,
    from_block_hash: BlockHash,
) -> anyhow::Result<bool> {
    let repository_hash = repositories
        .get_block_by_number(from_block_number)
        .with_context(|| format!("failed to read block {from_block_number} from local repository"))?
        .map(|b| b.hash());
    // SYSCOIN: repository persistence can lag the canonical replay WAL on restart, so keep the
    // rebuild/revert guard tied to the immutable replay source when available.
    let replay_hash = replay_storage.get_canonical_block_hash(from_block_number);
    let current_hash = match (replay_hash, repository_hash) {
        (Some(replay_hash), Some(repository_hash)) => {
            if replay_hash != repository_hash {
                tracing::warn!(
                    from_block_number,
                    ?from_block_hash,
                    ?replay_hash,
                    ?repository_hash,
                    "repository hash differs from replay WAL; using replay canonical hash for startup rebuild/revert guard"
                );
            }
            Some(replay_hash)
        }
        (Some(replay_hash), None) => {
            tracing::info!(
                from_block_number,
                ?from_block_hash,
                ?replay_hash,
                "repository is behind replay WAL; using replay canonical hash for startup rebuild/revert guard"
            );
            Some(replay_hash)
        }
        (None, Some(repository_hash)) => Some(repository_hash),
        (None, None) => None,
    };
    Ok(match current_hash {
        Some(hash) if hash == from_block_hash => true,
        Some(hash) => {
            if replay_wal_is_linked_from(replay_storage, from_block_number) {
                tracing::info!(
                    from_block_number,
                    current_hash = ?hash,
                    ?from_block_hash,
                    "skipping startup rebuild/revert: from_block_hash changed and replay WAL is linked from boundary"
                );
                false
            } else {
                // SYSCOIN: after a crash during a multi-block rebuild, the boundary block may
                // already have a new hash while later replay records still point to the old chain.
                // Keep the rebuild active so startup can finish rewriting the remaining WAL range.
                tracing::warn!(
                    from_block_number,
                    current_hash = ?hash,
                    ?from_block_hash,
                    "keeping startup rebuild/revert: boundary hash changed but replay WAL is not linked from boundary"
                );
                true
            }
        }
        None => {
            tracing::warn!(
                from_block_number,
                ?from_block_hash,
                "skipping startup rebuild/revert: block `from_block_number` not found in repository \
                 or replay WAL (check `from_block_number` is correct — it may be a typo or beyond the local tip)"
            );
            false
        }
    })
}

fn replay_wal_is_linked_from(
    replay_storage: &dyn ReadReplay,
    from_block_number: BlockNumber,
) -> bool {
    let Some(mut previous_hash) = replay_storage.get_canonical_block_hash(from_block_number) else {
        return false;
    };

    for block_number in (from_block_number + 1)..=replay_storage.latest_record() {
        let Some(context) = replay_storage.get_context(block_number) else {
            return false;
        };
        let Some(parent_hash) = context.block_hashes.0.last() else {
            return false;
        };
        if BlockHash::from(*parent_hash) != previous_hash {
            return false;
        }
        let Some(current_hash) = replay_storage.get_canonical_block_hash(block_number) else {
            return false;
        };
        previous_hash = current_hash;
    }

    true
}

/// Fetches the L1 state, performing any configured startup L1 revert first, and returns the
/// post-revert state.
async fn fetch_l1_state_with_startup_revert(
    config: &Config,
    node_role: NodeRole,
    rebuild: Option<&RebuildConfig>,
    l1_provider: &NodeProvider,
    gateway_provider: Option<&NodeProvider>,
    bridgehub_address: Address,
    chain_id: u64,
) -> anyhow::Result<L1State> {
    // The batcher node must wait for any pending L1 commit/prove/execute transactions (from a
    // prior run) to be mined before starting, so it doesn't conflict with itself. Non-batcher
    // consensus nodes never submit L1 transactions, so they don't need this wait: calling
    // fetch_finalized on them would spuriously fail when a concurrently running batcher node keeps
    // submitting new batch transactions.
    let use_finalized = node_role.is_main() && config.batcher_config.enabled;
    let l1_state = L1State::fetch_with_finality(
        use_finalized,
        l1_provider.clone(),
        gateway_provider.cloned(),
        bridgehub_address,
        chain_id,
        config.general_config.startup_sl_finalization_timeout,
    )
    .await
    .context("failed to fetch L1 state")?;

    if node_role.is_main()
        && let Some(rebuild) = rebuild
    {
        let sl_provider = if l1_state.l1_chain_id == l1_state.sl_chain_id {
            l1_provider.clone()
        } else {
            gateway_provider
                .cloned()
                .context("chain settles on Gateway but no gateway RPC provider is configured")?
        };

        let l1_revert_ran = revert_l1_on_startup(rebuild, config, &l1_state, &sl_provider)
            .await
            .context("startup l1 revert failed")?;

        if l1_revert_ran {
            // The revert invalidated the batch-finality numbers; re-fetch so the returned state
            // reflects the post-revert chain.
            return L1State::fetch_with_finality(
                use_finalized,
                l1_provider.clone(),
                gateway_provider.cloned(),
                bridgehub_address,
                chain_id,
                config.general_config.startup_sl_finalization_timeout,
            )
            .await
            .context("failed to fetch L1 state after startup revert");
        }
    }

    Ok(l1_state)
}

#[allow(clippy::too_many_arguments)]
async fn run_main_node_pipeline(
    config: &Config,
    sl_provider: NodeProvider,
    node_state_on_startup: NodeStateOnStartup,
    block_replay_storage: impl WriteReplay + Clone,
    runtime: &Runtime,
    state: impl ReadStateHistory + WriteState + Clone,
    starting_block: u64,
    rebuild_options: Option<RebuildOptions>,
    repositories: impl WriteRepository + Clone,
    block_context_provider: BlockContextProvider<impl L2Subpool>,
    tree: MerkleTree<RocksDBWrapper>,
    finality: impl ReadFinality + Clone,
    chain_id: u64,
    tx_acceptance_state_sender: watch::Sender<TransactionAcceptanceState>,
    sidecar_sender: tokio::sync::mpsc::Sender<BlobTransactionSidecar>,
    committed_batch_provider: CommittedBatchProvider,
    canonization_engine: BlockCanonizationEngine,
    leadership: LeadershipSignal,
    stop_receiver: watch::Receiver<bool>,
    commit_submitted_tx: watch::Sender<u64>,
    verify_request_tx: tokio::sync::mpsc::Sender<VerifyBatch>,
    verify_result_rx: tokio::sync::mpsc::Receiver<PeerVerifyBatchResult>,
    settles_on_gateway: bool,
    syscoin_edge_da_commit_target: Address,
    pubdata_mode: Option<PubdataMode>,
    replay_archiver: Option<impl ReplayArchiver>,
) -> watch::Receiver<TransactionAcceptanceState> {
    let priority_tree_db_path = config
        .general_config
        .rocks_db_path
        .join(PRIORITY_TREE_DB_NAME);
    let internal_config_manager = init_and_report_internal_config_manager(
        config
            .general_config
            .rocks_db_path
            .join(INTERNAL_CONFIG_FILE_NAME),
    );

    let monitor = BackpressureMonitor::new(config.build_backpressure_config(), stop_receiver);
    let pipeline_gate = monitor.subscribe_gate();

    let (replays_to_execute_sender, replays_to_execute) = tokio::sync::mpsc::unbounded_channel();
    let (applied_block_number_sender, applied_block_number_receiver) =
        watch::channel(initial_applied_block_number(&block_context_provider));

    let pipeline = Pipeline::new(runtime.clone())
        .pipe(ConsensusNodeCommandSource {
            block_replay_storage: block_replay_storage.clone(),
            starting_block,
            rebuild_options,
            replays_to_execute,
            pipeline_gate,
            leadership,
            produce_enabled: block_production_enabled(config),
        })
        .pipe(BlockExecutor {
            block_context_provider,
            state: state.clone(),
            config: config.into(),
            tx_acceptance_state_sender,
            applied_block_number_receiver,
        })
        .pipe(BlockCanonizer {
            consensus: canonization_engine,
            canonized_blocks_for_execution: replays_to_execute_sender,
        })
        .pipe(BlockApplier {
            state: state.clone(),
            replay: block_replay_storage.clone(),
            repositories: repositories.clone(),
            config: config.into(),
            applied_block_number_sender,
        })
        .pipe_opt(
            config
                .sequencer_config
                .revm_consistency_checker_enabled
                .then(|| {
                    RevmConsistencyChecker::new(
                        state.clone(),
                        internal_config_manager.clone(),
                        config
                            .sequencer_config
                            .revm_consistency_checker_revert_on_divergence,
                        config
                            .sequencer_config
                            .revm_consistency_checker_allow_bootstrap_skip,
                    )
                }),
        )
        .pipe(TreeManager { tree: tree.clone() });

    if !config.batcher_config.enabled {
        tracing::warn!(
            "Batcher subsystem disabled — skipping prover input generation, L1 settlement, and downstream components"
        );
        let pipeline = pipeline.pipe(NoOpSink::new());
        let components = pipeline.components();
        pipeline.spawn();
        runtime.spawn_critical_task(
            "clear failing block config",
            clear_failing_block_config_task(finality, internal_config_manager),
        );
        let snapshot_rx = PipelineTracker::spawn(runtime, components);
        return monitor.spawn(runtime, snapshot_rx);
    }
    let pubdata_mode = pubdata_mode
        .expect("effective pubdata mode must be set when the Main Node batcher is enabled");
    // SYSCOIN
    let batch_work_state_history_reserve = config
        .prover_input_generator_config
        .maximum_in_flight_blocks
        + <BatchWorkSource as PipelineComponent>::OUTPUT_CHANNEL_CAPACITY
        + <TreeManager as PipelineComponent>::OUTPUT_CHANNEL_CAPACITY
        + BLOCK_APPLIER_OUTPUT_BUFFER_RESERVE
        + REVM_CONSISTENCY_CHECKER_OUTPUT_BUFFER_RESERVE
        + EXECUTION_PIPELINE_IN_FLIGHT_STATE_RESERVE;
    let batch_work_channel_capacity = config
        .general_config
        .blocks_to_retain_in_memory
        .checked_sub(batch_work_state_history_reserve)
        .filter(|capacity| *capacity > 0)
        .unwrap_or_else(|| {
            panic!(
                "blocks_to_retain_in_memory ({}) must exceed async batch-work state history reserve ({batch_work_state_history_reserve})",
                config.general_config.blocks_to_retain_in_memory
            )
        })
        .min(MAX_BATCH_WORK_CHANNEL_CAPACITY);
    tracing::info!(
        batch_work_channel_capacity,
        blocks_to_retain_in_memory = config.general_config.blocks_to_retain_in_memory,
        batch_work_state_history_reserve,
        "Configured async batch-work queue capacity"
    );
    // SYSCOIN
    let batch_work_storage =
        BatchWorkStorage::new(config.general_config.rocks_db_path.join("batch_work_queue"))
            .expect("failed to initialize batch work storage");
    let (batch_work_tx, batch_work_rx) = tokio::sync::mpsc::channel(batch_work_channel_capacity);
    let bitcoin_da_status_storage = BitcoinDaStatusStorage::new(
        config
            .general_config
            .rocks_db_path
            .join("bitcoin_da_status"),
    )
    .expect("failed to initialize bitcoin da status storage");
    bitcoin_da_status_storage
        .delete_through(node_state_on_startup.l1_state.last_committed_batch)
        .await
        .expect("failed to prune stale bitcoin da status");

    tracing::info!("Initializing ProofStorage");
    let proof_storage = ProofStorage::new(config.prover_api_config.proof_storage.clone())
        .await
        .expect("Failed to initialize ProofStorage");

    let (fri_proving_step, fri_job_manager) = FriProvingPipelineStep::new(
        proof_storage.clone(),
        node_state_on_startup.l1_state.last_proved_batch,
        config.prover_api_config.fri_job_timeout,
        config.prover_api_config.max_assigned_batch_range,
    );
    // SYSCOIN
    let (snark_proving_step, snark_job_manager) = SnarkProvingPipelineStep::new(
        proof_storage.clone(),
        config.prover_api_config.max_fris_per_snark,
        node_state_on_startup.l1_state.last_proved_batch,
        node_state_on_startup.l1_state.last_committed_batch,
        config.prover_api_config.snark_job_timeout,
        config.prover_api_config.max_assigned_batch_range,
        committed_batch_provider.clone(),
    );

    if config.prover_api_config.enabled {
        // SYSCOIN: `prover_server` enforces this header when remote Basic Auth is configured.
        let prover_api_basic_auth = config.prover_api_config.basic_auth_header();
        runtime.spawn_critical_with_graceful_shutdown_signal("prover server", |shutdown| {
            prover_server::run(
                fri_job_manager.clone(),
                snark_job_manager.clone(),
                proof_storage.clone(),
                config.prover_api_config.address.clone(),
                prover_api_basic_auth.clone(),
                shutdown,
            )
        });
    }

    if config.prover_api_config.fake_fri_provers.enabled {
        run_fake_fri_provers(&config.prover_api_config, runtime, fri_job_manager);
    }

    if config.prover_api_config.fake_snark_provers.enabled {
        run_fake_snark_provers(&config.prover_api_config, runtime, snark_job_manager);
    }

    if !config.prover_input_generator_config.enable_input_generation {
        assert!(
            config.prover_api_config.fake_fri_provers.enabled
                && config.prover_api_config.fake_snark_provers.enabled,
            "prover_input_generator_config.enable_input_generation=false requires both \
             prover_api_config.fake_fri_provers.enabled and \
             prover_api_config.fake_snark_provers.enabled to be true"
        );
    }

    // SYSCOIN: upstream contracts do not expose canonical upgrade marker helpers. Fresh v31
    // deployments rely on the OS-recorded upgrade tx hash, so no startup override is required.
    let upgrade_batch_number = 0;
    let upgrade_tx_hash = None;

    // Pick the L1Sender config based on whether the chain is currently settling on Gateway:
    // when it is, gateway_sender operator keys and fee caps are used; otherwise the L1-targeted
    // l1_sender config is used.
    let commit_sender_config: zksync_os_l1_sender::config::L1SenderConfig<CommitCommand> =
        if settles_on_gateway {
            config.gateway_sender_config.clone().into()
        } else {
            config.l1_sender_config.clone().into()
        };
    let prove_sender_config: zksync_os_l1_sender::config::L1SenderConfig<ProofCommand> =
        if settles_on_gateway {
            config.gateway_sender_config.clone().into()
        } else {
            config.l1_sender_config.clone().into()
        };
    let execute_sender_config: zksync_os_l1_sender::config::L1SenderConfig<ExecuteCommand> =
        if settles_on_gateway {
            config.gateway_sender_config.clone().into()
        } else {
            config.l1_sender_config.clone().into()
        };

    // SYSCOIN
    let execution_pipeline = pipeline.pipe(BatchWorkDispatcher::new(
        batch_work_storage.clone(),
        batch_work_tx,
    ));

    let batch_pipeline = Pipeline::new(runtime.clone())
        .pipe(BatchWorkSource::new(batch_work_storage, batch_work_rx))
        .pipe(ProverInputGenerator {
            enable_logging: config.prover_input_generator_config.logging_enabled,
            maximum_in_flight_blocks: config
                .prover_input_generator_config
                .maximum_in_flight_blocks,
            read_state: state.clone(),
            pubdata_mode,
            merkle_tree: tree,
            runtime: runtime.clone(),
            compact_edge_da_commit_target: syscoin_edge_da_commit_target,
            disabled: !config.prover_input_generator_config.enable_input_generation,
        })
        .pipe(Batcher {
            startup_config: BatcherStartupConfig {
                last_committed_batch: node_state_on_startup.l1_state.last_committed_batch,
                last_executed_batch: node_state_on_startup.l1_state.last_executed_batch,
                upgrade_batch_number,
                upgrade_tx_hash,
                last_persisted_block: node_state_on_startup.block_replay_storage_last_block,
            },
            chain_id,
            sl_chain_id: node_state_on_startup.l1_state.sl_chain_id,
            chain_address_sl: node_state_on_startup.l1_state.diamond_proxy_address_sl(),
            compact_edge_da_commit_target: syscoin_edge_da_commit_target,
            pubdata_limit_bytes: config.sequencer_config.block_pubdata_limit_bytes,
            batcher_config: config.batcher_config.clone(),
            pubdata_mode,
            sidecar_sender,
            committed_batch_provider: committed_batch_provider.clone(),
            read_state: state.clone(),
            bitcoin_da_status_storage: bitcoin_da_status_storage.clone(),
        })
        .pipe(BatchVerificationPipelineStep::new(
            config.batch_verification_config.clone().into(),
            node_state_on_startup.l1_state.clone(),
            node_state_on_startup.l1_state.last_committed_batch,
            verify_request_tx,
            verify_result_rx,
        ))
        .pipe(fri_proving_step)
        .pipe(GaplessCommitter {
            next_expected_batch_number: node_state_on_startup.l1_state.last_executed_batch + 1,
            last_committed_batch_number: node_state_on_startup.l1_state.last_committed_batch,
            proof_storage,
            batch_verification_l1_config: node_state_on_startup.l1_state.batch_verification.clone(),
        })
        .pipe(UpgradeGatekeeper::new(
            node_state_on_startup.l1_state.diamond_proxy_sl.clone(),
        ))
        // SYSCOIN
        .pipe(BitcoinDaFinalityGate::new(
            config.batcher_config.clone(),
            bitcoin_da_status_storage.clone(),
            settles_on_gateway,
        ))
        .pipe_opt(replay_archiver.map(|replay_archiver| {
            ReplayArchiveGateComponent::new(replay_archiver, block_replay_storage.clone())
        }))
        .pipe(L1Sender::<CommitCommand> {
            provider: sl_provider.clone(),
            config: commit_sender_config,
            to_address: node_state_on_startup.l1_state.validator_timelock_sl,
            gateway: settles_on_gateway,
            commit_submitted_tx: Some(commit_submitted_tx),
            sl_block_number: node_state_on_startup.l1_state.sl_block_number,
        })
        // SYSCOIN
        .pipe(BitcoinDaStatusCleanup::new(bitcoin_da_status_storage))
        .pipe(snark_proving_step)
        .pipe(GaplessL1ProofSender::new(
            node_state_on_startup.l1_state.last_executed_batch + 1,
        ))
        .pipe(L1Sender::<ProofCommand> {
            provider: sl_provider.clone(),
            config: prove_sender_config,
            to_address: node_state_on_startup.l1_state.validator_timelock_sl,
            gateway: settles_on_gateway,
            commit_submitted_tx: None,
            sl_block_number: node_state_on_startup.l1_state.sl_block_number,
        })
        .pipe(
            PriorityTreePipelineStep::new(
                block_replay_storage.clone(),
                &priority_tree_db_path,
                finality,
                committed_batch_provider,
            )
            .unwrap(),
        )
        .pipe(L1Sender {
            provider: sl_provider,
            config: execute_sender_config,
            to_address: node_state_on_startup.l1_state.validator_timelock_sl,
            gateway: settles_on_gateway,
            commit_submitted_tx: None,
            sl_block_number: node_state_on_startup.l1_state.sl_block_number,
        })
        .pipe(BatchSink::new(internal_config_manager));

    let mut components = execution_pipeline.components();
    components.extend(batch_pipeline.components());
    tracing::info!("Launching execution pipeline");
    execution_pipeline.spawn();
    tracing::info!("Launching batch pipeline");
    batch_pipeline.spawn();
    let snapshot_rx = PipelineTracker::spawn(runtime, components);
    monitor.spawn(runtime, snapshot_rx)
}

/// Only for EN - we still populate channels destined for the batcher subsystem -
/// need to drain them to not get stuck
#[allow(clippy::too_many_arguments)]
async fn run_en_pipeline(
    config: &Config,
    replays_for_sequencer: tokio::sync::mpsc::Receiver<ReplayRecord>,
    committed_batch_provider: CommittedBatchProvider,
    node_state_on_startup: NodeStateOnStartup,
    block_replay_storage: impl WriteReplay + Clone,
    runtime: &Runtime,
    block_context_provider: BlockContextProvider<impl L2Subpool>,
    state: impl ReadStateHistory + WriteState + Clone,
    tree: MerkleTree<RocksDBWrapper>,
    repositories: impl WriteRepository + Clone,
    finality: impl ReadFinality + Clone,
    stop_receiver: watch::Receiver<bool>,
    tx_acceptance_state_sender: watch::Sender<TransactionAcceptanceState>,
    chain_id: u64,
    syscoin_edge_da_commit_target: Address,
    verify_batch_rx: tokio::sync::mpsc::Receiver<PeerVerifyBatch>,
    outgoing_verify_results: tokio::sync::broadcast::Sender<PeerVerifyBatchResult>,
) -> watch::Receiver<TransactionAcceptanceState> {
    let internal_config_manager = init_and_report_internal_config_manager(
        config
            .general_config
            .rocks_db_path
            .join(INTERNAL_CONFIG_FILE_NAME),
    );
    let (applied_block_number_sender, applied_block_number_receiver) =
        watch::channel(initial_applied_block_number(&block_context_provider));

    let monitor =
        BackpressureMonitor::new(config.build_backpressure_config(), stop_receiver.clone());
    let pipeline_gate = monitor.subscribe_gate();

    let pipeline = Pipeline::new(runtime.clone())
        .pipe(ExternalNodeCommandSource {
            replays_for_sequencer,
            up_to_block: config.sequencer_config.en_sync_up_to_block,
            pipeline_gate,
        })
        .pipe(BlockExecutor {
            block_context_provider,
            state: state.clone(),
            config: config.into(),
            tx_acceptance_state_sender,
            applied_block_number_receiver,
        })
        .pipe(BlockApplier {
            state: state.clone(),
            replay: block_replay_storage.clone(),
            repositories: repositories.clone(),
            config: config.into(),
            applied_block_number_sender,
        })
        .pipe_opt(
            config
                .sequencer_config
                .revm_consistency_checker_enabled
                .then(|| {
                    RevmConsistencyChecker::new(
                        state.clone(),
                        internal_config_manager.clone(),
                        config
                            .sequencer_config
                            .revm_consistency_checker_revert_on_divergence,
                        config
                            .sequencer_config
                            .revm_consistency_checker_allow_bootstrap_skip,
                    )
                }),
        )
        .pipe(TreeManager { tree: tree.clone() });

    // SYSCOIN: construct the batch-verification responder only when this EN is
    // explicitly configured to sign batches. Our default signing key is empty
    // unless `client_enabled=true`, and `pipe_if` evaluates both branch values.
    let pipeline = if config.batch_verification_config.client_enabled {
        pipeline.pipe(BatchVerificationResponder::new(
            chain_id,
            node_state_on_startup.l1_state.diamond_proxy_address_sl(),
            config.batch_verification_config.signing_key.clone(),
            syscoin_da_verification_config(config),
            finality.clone(),
            node_state_on_startup.l1_state.clone(),
            syscoin_edge_da_commit_target,
            state.clone(),
            verify_batch_rx,
            outgoing_verify_results,
        ))
    } else {
        pipeline.pipe(NoOpSink::new())
    };

    let components = pipeline.components();
    pipeline.spawn();
    let snapshot_rx = PipelineTracker::spawn(runtime, components);

    if config.general_config.run_priority_tree {
        let priority_tree_manager = PriorityTreeManager::new(
            block_replay_storage,
            Path::new(
                &config
                    .general_config
                    .rocks_db_path
                    .join(PRIORITY_TREE_DB_NAME),
            ),
            finality.clone(),
            committed_batch_provider,
        )
        .unwrap();
        runtime.spawn_critical_with_graceful_shutdown_signal(
            "priority tree caching",
            |shutdown| async move {
                let (reporter, _rx) =
                    zksync_os_observability::ComponentStateReporter::new("priority_tree");
                tokio::select! {
                    result = priority_tree_manager.run(None, reporter) => {
                        result.expect("PriorityTreeManager run failed");
                    }
                    _guard = shutdown => {
                    }
                }
            },
        );
    }
    runtime.spawn_critical_task(
        "clear failing block config",
        clear_failing_block_config_task(finality, internal_config_manager),
    );
    monitor.spawn(runtime, snapshot_rx)
}

fn init_and_report_internal_config_manager(
    internal_config_path: std::path::PathBuf,
) -> InternalConfigManager {
    let internal_config_manager = InternalConfigManager::new(internal_config_path)
        .expect("Failed to initialize InternalConfigManager");

    // Report blacklisted addresses metric
    let internal_config = internal_config_manager
        .read_config()
        .expect("Failed to read internal config");
    GENERAL_METRICS
        .blacklisted_addresses_count
        .set(internal_config.l2_signer_blacklist.len());

    internal_config_manager
}

/// Warns when the main node's batch verification server threshold is lower than the
/// threshold configured on L1.
///
/// This is a startup sanity check only: the pipeline later enforces the effective threshold by
/// taking the max(server.threshold, l1.threshold).
///
/// In practice, it means that the server operator expectation and the L1 state are mismatched.
fn check_batch_verification_mismatch(
    server_config: &config::BatchVerificationConfig,
    l1_config: &BatchVerificationSL,
) -> bool {
    if !server_config.server_enabled {
        return false;
    }
    let l1_threshold = match l1_config {
        BatchVerificationSL::Enabled(config) => config.threshold,
        BatchVerificationSL::Disabled => return false,
    };

    if server_config.threshold < l1_threshold {
        tracing::warn!(
            configured_threshold = server_config.threshold,
            l1_threshold,
            "Batch verification server threshold is lower than the L1 threshold; consider increasing the server threshold"
        );
        return true;
    }
    false
}

// SYSCOIN
fn validate_batch_verification_startup_policy(
    server_config: &config::BatchVerificationConfig,
    l1_config: &BatchVerificationSL,
) {
    if !server_config.server_enabled {
        return;
    }

    let l1_policy_overrides_local_signers = match l1_config {
        BatchVerificationSL::Enabled(config) => {
            !config.validators.is_empty() || config.threshold > 0
        }
        BatchVerificationSL::Disabled => false,
    };

    if !l1_policy_overrides_local_signers && server_config.accepted_signers.is_empty() {
        panic!(
            "`batch_verification.accepted_signers` requires at least one accepted signer when \
             `batch_verification.server_enabled=true` and no L1 batch-verification policy is configured"
        );
    }
}

/// Returns the pubdata mode used by all block-producing components on the Main Node, taking
/// settlement-layer discovery into account: when the chain settles on Gateway, the mode is
/// derived from the gateway's DA input mode (`Rollup` → [`PubdataMode::RelayedL2Calldata`],
/// `Validium` → [`PubdataMode::Validium`]); when it settles on L1, the configured
/// `l1_sender.pubdata_mode` is used (and its presence is enforced here).
fn effective_main_node_pubdata_mode(
    config: &Config,
    settles_on_gateway: bool,
    da_input_mode: BatchDaInputMode,
) -> PubdataMode {
    if settles_on_gateway {
        match da_input_mode {
            BatchDaInputMode::Rollup => PubdataMode::RelayedL2Calldata,
            BatchDaInputMode::Validium => PubdataMode::Validium,
        }
    } else {
        config
            .l1_sender_config
            .pubdata_mode
            .expect("`l1_sender.pubdata_mode` is required on the Main Node when settling on L1")
    }
}

/// Validates that the operator keys required for the L1Sender pipeline are present in config,
/// based on the settlement layer discovered at startup. When settling on L1, `l1_sender.operator_*_sk`
/// are required; when settling on Gateway, `gateway_sender.operator_*_sk` are required. Reports all
/// missing keys at once via panic so the operator can fix them in a single restart.
fn check_required_operator_keys(config: &Config, settles_on_gateway: bool) {
    let (section, missing): (&str, Vec<&str>) = if settles_on_gateway {
        let gw = &config.gateway_sender_config;
        let mut missing = vec![];
        if gw.operator_commit_sk.is_none() {
            missing.push("operator_commit_sk");
        }
        if gw.operator_prove_sk.is_none() {
            missing.push("operator_prove_sk");
        }
        if gw.operator_execute_sk.is_none() {
            missing.push("operator_execute_sk");
        }
        ("gateway_sender", missing)
    } else {
        let l1 = &config.l1_sender_config;
        let mut missing = vec![];
        if l1.operator_commit_sk.is_none() {
            missing.push("operator_commit_sk");
        }
        if l1.operator_prove_sk.is_none() {
            missing.push("operator_prove_sk");
        }
        if l1.operator_execute_sk.is_none() {
            missing.push("operator_execute_sk");
        }
        ("l1_sender", missing)
    };
    if !missing.is_empty() {
        let target = if settles_on_gateway { "Gateway" } else { "L1" };
        let formatted = missing
            .iter()
            .map(|k| format!("`{section}.{k}`"))
            .collect::<Vec<_>>()
            .join(", ");
        panic!(
            "missing operator keys required for settling on {target}: {formatted}. \
             Set them in the `{section}` config section."
        );
    }
}

fn resolve_syscoin_edge_da_commit_target(
    l1_state: &L1State,
    settles_on_gateway: bool,
    required: bool,
) -> Address {
    let live_target = l1_state.validator_timelock_sl;
    if required {
        assert_ne!(
            live_target,
            Address::ZERO,
            "Gateway ValidatorTimelock must be available when compact edge DA is active"
        );
    }

    if let Some(expected) = syscoin_edge_da_commit_target_from_env() {
        assert_ne!(
            expected,
            Address::ZERO,
            "SYSCOIN_EDGE_DA_COMMIT_TARGET must be nonzero"
        );
        if settles_on_gateway {
            assert_eq!(
                live_target, expected,
                "SYSCOIN edge DA commit target mismatch: live Gateway ValidatorTimelock is {}, \
                 but SYSCOIN_EDGE_DA_COMMIT_TARGET is {}",
                live_target, expected
            );
        }
        return expected;
    }

    live_target
}

fn syscoin_edge_da_commit_target_from_env() -> Option<Address> {
    std::env::var("SYSCOIN_EDGE_DA_COMMIT_TARGET")
        .or_else(|_| std::env::var("ZKSYNC_OS_SYSCOIN_EDGE_DA_COMMIT_TARGET"))
        .ok()
        .map(|value| parse_syscoin_edge_da_commit_target(&value))
}

fn parse_syscoin_edge_da_commit_target(value: &str) -> Address {
    let value = value.trim();
    value.parse::<Address>().unwrap_or_else(|err| {
        panic!("invalid SYSCOIN_EDGE_DA_COMMIT_TARGET `{value}`: {err}");
    })
}

async fn commit_proof_execute_block_numbers(
    l1_state: &L1State,
    committed_batch_provider: &CommittedBatchProvider,
) -> (u64, u64, u64, u64) {
    let last_committed_block = if l1_state.last_committed_batch == 0 {
        0
    } else {
        committed_batch_provider
            .get(l1_state.last_committed_batch)
            .expect("last_committed_batch is expected to be loaded")
            .last_block_number()
    };

    // only used to log on node startup
    let last_proved_block = if l1_state.last_proved_batch == 0 {
        0
    } else {
        committed_batch_provider
            .get(l1_state.last_proved_batch)
            .expect("last_proved_batch is expected to be loaded")
            .last_block_number()
    };

    let last_executed_block = if l1_state.last_executed_batch == 0 {
        0
    } else {
        committed_batch_provider
            .get(l1_state.last_executed_batch)
            .expect("last_executed_batch is expected to be loaded")
            .last_block_number()
    };
    let last_finalized_executed_block = if l1_state.last_finalized_executed_batch == 0 {
        0
    } else {
        committed_batch_provider
            .get(l1_state.last_finalized_executed_batch)
            .expect("last_finalized_executed_batch is expected to be loaded")
            .last_block_number()
    };
    (
        last_committed_block,
        last_proved_block,
        last_executed_block,
        last_finalized_executed_block,
    )
}

fn run_fake_snark_provers(
    config: &ProverApiConfig,
    runtime: &Runtime,
    snark_job_manager: Arc<SnarkJobManager>,
) {
    tracing::info!(
        max_batch_age = ?config.fake_snark_provers.max_batch_age,
        "Initializing fake SNARK prover"
    );
    let fake_snark_prover = FakeSnarkProver::new(
        snark_job_manager.clone(),
        config.fake_snark_provers.max_batch_age,
    );
    runtime.spawn_critical_task("fake snark prover", fake_snark_prover.run());
}

fn run_fake_fri_provers(
    config: &ProverApiConfig,
    runtime: &Runtime,
    fri_job_manager: Arc<FriJobManager>,
) {
    tracing::info!(
        workers = config.fake_fri_provers.workers,
        compute_time = ?config.fake_fri_provers.compute_time,
        min_task_age = ?config.fake_fri_provers.min_age,
        timeout_frequency = ?config.fake_fri_provers.timeout_frequency,
        "Initializing fake FRI provers"
    );
    let fake_provers_pool = FakeFriProversPool::new(
        fri_job_manager.clone(),
        config.fake_fri_provers.workers,
        config.fake_fri_provers.compute_time,
        config.fake_fri_provers.min_age,
        config.fake_fri_provers.timeout_frequency,
    );
    fake_provers_pool.spawn(runtime);
}

/// Determines the block for node to start from.
///
/// Panics if no batches are committed to L1 yet.
fn determine_starting_block(
    config: &Config,
    node_startup_state: &NodeStateOnStartup,
    state: &impl ReadStateHistory,
    last_matching_block: BlockNumber,
    rebuild_options: Option<&RebuildOptions>,
) -> BlockNumber {
    assert!(
        node_startup_state.l1_state.last_committed_batch > 0,
        "No batches committed to L1 yet - start with block/batch 1"
    );

    let desired_starting_block = if let Some(forced_starting_block_number) =
        config.general_config.force_starting_block_number
    {
        forced_starting_block_number
    } else {
        // Start with the oldest block from:
        let want_to_start_from = [
            // To ensure consistency/correctness, we want to replay at least `config.min_blocks_to_replay` blocks
            node_startup_state
                .block_replay_storage_last_block
                .saturating_sub(config.general_config.min_blocks_to_replay as u64),
            // We need to replay old unexecuted blocks to rebuild and execute the batches they are in
            node_startup_state.last_l1_executed_block,
            // Repositories' persistence may have fallen behind - we need to replay blocks to rebuild it
            node_startup_state.repositories_persisted_block,
            // In the current tree implementation this will always be ahead of `last_l1_executed_block`,
            // but this may change if we make tree persistence async (like elsewhere)
            node_startup_state.tree_last_block,
            // For compacted state, we need to replay all blocks that were not persisted yet.
            // For FullDiffs state (default) - this is always ahead of `last_l1_executed_block`.
            *state.block_range_available().end(),
            // If block rebuild (aka block reversion) is configured, we should ensure we replay
            // all the blocks we are rebuilding
            rebuild_options.map_or(u64::MAX, |opts| opts.from_block_number),
        ]
        .into_iter()
        .min()
        .unwrap();

        if last_matching_block < want_to_start_from {
            tracing::warn!(
                last_matching_block,
                want_to_start_from,
                "Node's blocks diverged from main node's blocks. Starting from last matching block + 1."
            );
        }

        last_matching_block.min(want_to_start_from)
    };

    // Ignore genesis here as we never actually run it in sequencer
    if desired_starting_block > 0
        && desired_starting_block < state.block_range_available().start() + 1
    {
        // This may only happen with Compacted State. This means that the block we want to rerun was already compacted.
        // This can be fixed by manually removing the storage persistence - which will force the node to start from block 1.

        // Alternatively, we can clear storage programmatically here and start from 1 - this is not currently implemented
        panic!(
            "Cannot start: desired_starting_block < state.block_range_available().start() + 1: {} < {}",
            desired_starting_block,
            state.block_range_available().start() + 1
        );
    }

    desired_starting_block
}

/// Finds the last block number where the local node's block hash matches the main node's block hash.
async fn find_last_matching_main_node_block(
    repo: &RepositoryManager,
    main_node_client: &MainNodeClient,
) -> anyhow::Result<u64> {
    async fn check(
        repo: &RepositoryManager,
        main_node_client: &MainNodeClient,
        block_number: u64,
    ) -> anyhow::Result<bool> {
        let local_hash = repo
            .get_block_by_number(block_number)?
            .map(|b| b.hash())
            .with_context(|| format!("Local node is missing block {block_number}"))?;
        if let Some(remote_block) = main_node_client
            .block_by_number(block_number.into(), false)
            .await?
        {
            Ok(local_hash == remote_block.hash())
        } else {
            // Main node is missing this block in RPC, assume there is a divergence.
            //
            // If we happen to query main node during restart it might not have this block in RPC
            // yet but have it in replay storage. Should still be fine to assume there is a divergence
            // in such cases. Ideally, we should be able to query main node's replay state through
            // interactive replay transport instead.
            Ok(false)
        }
    }

    let last_block = repo.get_latest_block();
    // Check last block first. Unless there was a reorg recently, this should return quickly.
    if check(repo, main_node_client, last_block).await? {
        return Ok(last_block);
    }
    if !check(repo, main_node_client, 0).await? {
        panic!("Genesis block mismatch between EN and main node");
    }

    // Binary search for the last matching block.
    let mut left = 0u64;
    let mut right = last_block;
    while left < right {
        #[allow(clippy::manual_div_ceil)]
        let mid = (left + right + 1) / 2;
        if check(repo, main_node_client, mid).await? {
            left = mid;
        } else {
            right = mid - 1;
        }
    }
    Ok(left)
}

fn prepare_raft_storage(config: &Config) -> anyhow::Result<()> {
    let raft_storage_path = config.general_config.rocks_db_path.join(RAFT_DB_NAME);
    if config.consensus_config.force_clear_raft_history
        && raft_storage_path_exists(&raft_storage_path)?
    {
        tracing::warn!(
            path = %raft_storage_path.display(),
            "force-clearing persisted raft history before startup"
        );
        // Use DB::destroy rather than remove_dir_all so that only files RocksDB
        // tracks are removed; an arbitrary path misconfiguration cannot wipe more.
        zksync_os_rocksdb::rocksdb::DB::destroy(
            &zksync_os_rocksdb::rocksdb::Options::default(),
            &raft_storage_path,
        )
        .with_context(|| {
            format!(
                "failed to destroy raft storage at {}",
                raft_storage_path.display()
            )
        })?;
        // DB::destroy leaves behind an empty directory; remove it so the next
        // open starts completely clean (RocksDB recreates the dir on open).
        let _ = std::fs::remove_dir(&raft_storage_path);
    }

    if !config.consensus_config.enabled && raft_storage_path_exists(&raft_storage_path)? {
        anyhow::bail!(
            "consensus is disabled but persisted raft history exists at {}; \
             either re-enable consensus or set `consensus.force_clear_raft_history=true` \
             to delete stale raft state before startup",
            raft_storage_path.display()
        );
    }

    Ok(())
}

fn raft_storage_path_exists(path: &Path) -> anyhow::Result<bool> {
    path.try_exists().with_context(|| {
        format!(
            "failed to check whether raft storage exists at {}",
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::{
        check_batch_verification_mismatch, initial_transaction_acceptance_state,
        validate_batch_verification_startup_policy,
    };
    use crate::config::BatchVerificationConfig;
    use alloy::primitives::address;
    use zksync_os_contract_interface::l1_discovery::{
        BatchVerificationSL, BatchVerificationSLConfig,
    };
    use zksync_os_types::{NodeRole, NotAcceptingReason, TransactionAcceptanceState};

    #[test]
    fn main_node_zero_block_cap_rejects_transactions_at_startup() {
        assert!(matches!(
            initial_transaction_acceptance_state(NodeRole::MainNode, Some(0), true),
            TransactionAcceptanceState::NotAccepting(reasons)
                if reasons == vec![NotAcceptingReason::BlockProductionDisabled]
        ));
    }

    #[test]
    fn main_node_disabled_batcher_rejects_transactions_at_startup() {
        assert!(matches!(
            initial_transaction_acceptance_state(NodeRole::MainNode, None, false),
            TransactionAcceptanceState::NotAccepting(reasons)
                if reasons == vec![NotAcceptingReason::BlockProductionDisabled]
        ));
    }

    #[test]
    fn main_node_positive_block_cap_accepts_transactions_at_startup() {
        assert!(matches!(
            initial_transaction_acceptance_state(NodeRole::MainNode, Some(1), true),
            TransactionAcceptanceState::Accepting
        ));
    }

    #[test]
    fn external_node_zero_block_cap_accepts_for_forwarding() {
        assert!(matches!(
            initial_transaction_acceptance_state(NodeRole::ExternalNode, Some(0), true),
            TransactionAcceptanceState::Accepting
        ));
    }

    #[test]
    fn test_batch_verification_is_disabled_on_server() {
        let server_config = BatchVerificationConfig::default();
        let l1_config = BatchVerificationSL::Enabled(BatchVerificationSLConfig {
            threshold: 0,
            validators: vec![address!("0x0000000000000000000000000000000000000001")],
        });
        let warned = check_batch_verification_mismatch(&server_config, &l1_config);
        assert!(!warned);
    }

    #[test]
    fn test_batch_verification_is_disabled_on_l1() {
        let config = BatchVerificationConfig {
            server_enabled: true,
            ..Default::default()
        };
        let warned = check_batch_verification_mismatch(&config, &BatchVerificationSL::Disabled);
        assert!(!warned);
    }

    #[test]
    fn test_batch_verification_is_mismatched() {
        let server_config = BatchVerificationConfig {
            server_enabled: true,
            threshold: 2,
            ..Default::default()
        };
        let l1_config = BatchVerificationSL::Enabled(BatchVerificationSLConfig {
            threshold: 3,
            validators: vec![
                address!("0x0000000000000000000000000000000000000001"),
                address!("0x0000000000000000000000000000000000000002"),
                address!("0x0000000000000000000000000000000000000003"),
                address!("0x0000000000000000000000000000000000000004"),
            ],
        });
        let warned = check_batch_verification_mismatch(&server_config, &l1_config);

        assert!(warned);
    }

    #[test]
    fn test_batch_verification_happy_path() {
        let server_config = BatchVerificationConfig {
            server_enabled: true,
            threshold: 3,
            ..Default::default()
        };
        let l1_config = BatchVerificationSL::Enabled(BatchVerificationSLConfig {
            threshold: 2,
            validators: vec![
                address!("0x0000000000000000000000000000000000000001"),
                address!("0x0000000000000000000000000000000000000002"),
                address!("0x0000000000000000000000000000000000000003"),
            ],
        });
        let warned = check_batch_verification_mismatch(&server_config, &l1_config);

        assert!(!warned);
    }

    // SYSCOIN
    #[test]
    #[should_panic(
        expected = "`batch_verification.accepted_signers` requires at least one accepted signer"
    )]
    fn test_batch_verification_requires_local_signers_without_l1_policy() {
        let server_config = BatchVerificationConfig {
            server_enabled: true,
            accepted_signers: vec![],
            ..Default::default()
        };

        validate_batch_verification_startup_policy(&server_config, &BatchVerificationSL::Disabled);
    }

    // SYSCOIN
    #[test]
    fn test_batch_verification_allows_empty_local_signers_with_l1_policy() {
        let server_config = BatchVerificationConfig {
            server_enabled: true,
            accepted_signers: vec![],
            ..Default::default()
        };
        let l1_config = BatchVerificationSL::Enabled(BatchVerificationSLConfig {
            threshold: 1,
            validators: vec![address!("0x0000000000000000000000000000000000000001")],
        });

        validate_batch_verification_startup_policy(&server_config, &l1_config);
    }
}
