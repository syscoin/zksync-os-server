pub mod commands;
pub mod config;
mod metrics;
pub mod pipeline_component;
pub mod upgrade_gatekeeper;

use crate::commands::{L1SenderCommand, SendToL1};
use crate::config::{L1SenderConfig, L1SenderFeeConfig, SYSCOIN_L1_PRIORITY_FEE_FLOOR_WEI};
use crate::metrics::{L1_SENDER_METRICS, PriorityFeeEstimatePercentile, PriorityFeeEstimateWindow};
use crate::pipeline_component::L1Sender;
use alloy::consensus::Transaction as ConsensusTransaction;
use alloy::eips::eip2718::Encodable2718;
use alloy::eips::eip4844::{DATA_GAS_PER_BLOB, env_settings::EnvKzgSettings};
use alloy::eips::eip7594::BlobTransactionSidecarVariant;
use alloy::eips::{BlockId, BlockNumberOrTag};
use alloy::network::{
    BlockResponse, NetworkTransactionBuilder, TransactionBuilder, TransactionBuilder4844,
    TransactionResponse,
};
use alloy::primitives::utils::{format_ether, format_units};
use alloy::primitives::{Address, B256, U256};
use alloy::providers::Provider;
use alloy::providers::ext::DebugApi;
use alloy::providers::utils::Eip1559Estimation;
use alloy::rpc::types::simulate::{SimBlock, SimulatePayload};
use alloy::rpc::types::state::{AccountOverride, StateOverridesBuilder};
use alloy::rpc::types::trace::geth::{CallConfig, GethDebugTracingOptions};
use alloy::rpc::types::{TransactionReceipt, TransactionRequest};
use alloy::transports::TransportError;
use anyhow::Context as _;
use futures::FutureExt;
use futures::future::BoxFuture;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};
use zksync_os_alloy_ext::dyn_wallet_provider::{EthDynProvider, EthWalletProvider};
use zksync_os_batch_types::batcher_model::{FriProof, SignedBatchEnvelope};
use zksync_os_observability::{ComponentStateReporter, GenericComponentState, StateLabel};
use zksync_os_operator_signer::SignerConfig;
use zksync_os_pipeline::{ComponentId, PeekableReceiver, SendAndRecordExt};

/// Component-specific state for the L1 sender.
pub enum L1SenderState {
    /// Waiting for the next batch to commit/prove/execute.
    Idle,
    /// Submitting a transaction to L1.
    SendingToL1,
    /// Transaction submitted; waiting for L1 block inclusion.
    WaitingL1Inclusion,
}

impl StateLabel for L1SenderState {
    fn generic(&self) -> GenericComponentState {
        match self {
            Self::Idle => GenericComponentState::Idle,
            Self::SendingToL1 => GenericComponentState::Active,
            Self::WaitingL1Inclusion => GenericComponentState::Active,
        }
    }
    fn specific(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::SendingToL1 => "sending_to_l1",
            Self::WaitingL1Inclusion => "waiting_l1_inclusion",
        }
    }
}

/// A code for "method not found" error response as declared in JSON-RPC 2.0 spec.
const METHOD_NOT_FOUND_CODE: i64 = -32601;
/// SYSCOIN: future that resolves into a (fallible) transaction receipt wait outcome.
/// The outcome distinguishes confirmed txs from dropped txs so delayed inclusion is non-fatal.
type TransactionReceiptFuture = BoxFuture<'static, anyhow::Result<ReceiptWaitOutcome>>;
// SYSCOIN: track the optional raw signed tx, current hash, nonce, and submission L1 block so
// dropped txs can be rebroadcast or recovered without crashing the L1 sender.
type PendingTx<Input> = (
    TransactionReceiptFuture,
    Input,
    Instant,
    Option<Vec<u8>>,
    B256,
    u64,
    u64,
);

// SYSCOIN: non-fatal receipt wait result used to recover from L1 mempool eviction
// and visible-but-stale transactions.
enum ReceiptWaitOutcome {
    Confirmed(TransactionReceipt),
    Dropped,
    TimedOut,
}

const REQUIRED_CONFIRMATIONS_L1: u64 = 3;
/// In case there's only one chain connected to gateway, it is very likely that there will be not enough block production
/// to reach 3 confirmations for such transactions
const REQUIRED_CONFIRMATIONS_GATEWAY: u64 = 1;
const OPERATOR_METRICS_POLL_INTERVAL: Duration = Duration::from_secs(60);
/// SYSCOIN Extra headroom over the L1 RPC gas estimate.
const L1_TX_GAS_ESTIMATE_PADDING_NUMERATOR: u64 = 120;
const L1_TX_GAS_ESTIMATE_PADDING_DENOMINATOR: u64 = 100;
/// Per-tx gas limit used when `eth_simulateV1` cannot produce a usable estimate.
/// Sized to cover the bounded set of commit/prove/execute calls.
const L1_GAS_LIMIT_FALLBACK: u64 = 15_000_000;

#[derive(Debug, Clone, Copy)]
struct FeeParams {
    max_fee_per_gas: u128,
    max_priority_fee_per_gas: u128,
    max_fee_per_blob_gas: u128,
}

impl<Input> L1Sender<Input>
where
    Input: SendToL1 + Send + 'static,
{
    pub async fn operator_address(&self) -> anyhow::Result<Address> {
        self.config.operator_signer.address().await
    }

    pub async fn run_l1_sender(
        self,
        inbound: PeekableReceiver<L1SenderCommand<Input>>,
        outbound: mpsc::Sender<SignedBatchEnvelope<FriProof>>,
        state_reporter: ComponentStateReporter,
    ) -> anyhow::Result<()> {
        run_l1_sender(
            inbound,
            outbound,
            self.to_address,
            self.provider,
            self.config,
            self.gateway,
            state_reporter,
            self.commit_submitted_tx,
            self.sl_block_number,
        )
        .await
    }
}

/// Process responsible for sending transactions to L1.
/// Handles one type of l1 command (e.g. Commit or Prove).
/// Loads up to `command_limit` commands from the channel and sends them to L1 in parallel.
/// Waits for all transactions to be mined, sends them to the output channel
/// and then starts with the next `command_limit` commands.
///
/// Important: the same provider (sender address) must not be used outside this process.
///     Otherwise, there will be a nonce conflict and a failed L1 transaction
///     (recoverable on restart)
///
/// Known issues:
///   * Crashes when there is a gap in incoming L1 blocks (happens periodically with Infura provider)
///
/// Note: we pass `to_address` - L1 contract address to send transactions to.
/// It differs between commit/prove/execute (e.g., timelock vs diamond proxy)
#[allow(clippy::too_many_arguments)]
pub async fn run_l1_sender<Input: SendToL1 + Send + 'static>(
    // == plumbing ==
    mut inbound: PeekableReceiver<L1SenderCommand<Input>>,
    outbound: mpsc::Sender<SignedBatchEnvelope<FriProof>>,

    // == command-specific settings ==
    to_address: Address,

    // == config ==
    mut provider: EthDynProvider,
    config: L1SenderConfig<Input>,
    gateway: bool,
    state_reporter: ComponentStateReporter,
    commit_submitted_tx: Option<watch::Sender<u64>>,
    // The SL block number at which `getTotalBatches*` was called on startup. Pinning the
    // confirmed-nonce baseline to this block ensures it is consistent with where the
    // inbound command queue begins — avoiding a crash caused by txs that are mined between
    // the `getTotalBatches` call and the nonce check.
    sl_block_number: u64,
) -> anyhow::Result<()> {
    let command_name = Input::COMPONENT_ID.as_str();
    config.fee_config.validate_syscoin_fee_caps()?;
    let force_transaction_resubmission = config.force_transaction_resubmission;

    // SYSCOIN: keep `config` available after operator registration because dropped-tx recovery
    // can resubmit commands through the same config.
    let operator_address =
        register_operator::<Input>(&mut provider, config.operator_signer.clone()).await?;
    let mut cmd_buffer = Vec::with_capacity(config.command_limit);
    // Process all potential passthrough commands first
    if process_prepending_passthrough_commands(
        &mut inbound,
        &outbound,
        &state_reporter,
        command_name,
    )
    .await?
    .is_none()
    {
        tracing::info!("inbound channel closed");
        return Ok(());
    }

    // On startup, either recover submitted transactions from a previous session or, when
    // explicitly requested, skip recovery so the normal send path replaces them.
    let recovered = if force_transaction_resubmission {
        vec![]
    } else {
        match recover_in_flight_txs(
            &provider,
            operator_address,
            gateway,
            &mut inbound,
            command_name,
            sl_block_number,
            &state_reporter,
        )
        .await
        {
            Ok(paired) => paired,
            Err(err) => {
                tracing::warn!("Error during in-flight transaction recovery: {err}");
                vec![]
            }
        }
    };

    // Wait for any recovered in-flight transactions to be mined before accepting
    // new commands. Their nonces precede anything we are about to send, so they
    // must be confirmed first.
    if !recovered.is_empty() {
        let pending_txs: Vec<PendingTx<Input>> = recovered
            .into_iter()
            .map(|(tx_hash, cmd, nonce)| {
                // SYSCOIN: recovered commit txs were submitted by a prior session but are now
                // owned by this sender loop. Announce them before waiting so the commit watcher
                // does not classify their eventual L1 event as an unexpected external commit.
                notify_commit_submitted(&commit_submitted_tx, &cmd);
                let fut = wait_for_confirmed_receipt(
                    provider.root().clone(),
                    tx_hash,
                    if gateway {
                        REQUIRED_CONFIRMATIONS_GATEWAY
                    } else {
                        REQUIRED_CONFIRMATIONS_L1
                    },
                    config.transaction_timeout,
                    config.tx_liveness_poll_interval,
                    config.tx_liveness_max_missing_polls,
                )
                .boxed();
                // SYSCOIN: recovered in-flight txs have no raw signed payload; if they disappear,
                // recovery resubmits from the queued command instead.
                (
                    fut,
                    cmd,
                    Instant::now(),
                    None,
                    tx_hash,
                    nonce,
                    sl_block_number,
                )
            })
            .collect();
        wait_for_txs_and_forward(
            pending_txs,
            &provider,
            operator_address,
            to_address,
            &config,
            gateway,
            &commit_submitted_tx,
            command_name,
            &state_reporter,
            &outbound,
        )
        .await?;
    }

    // At this point, recovered in-flight transactions are confirmed. If force resubmission is
    // enabled, only commands already queued at startup need replacement pricing.
    // Only actual SendToL1 commands are expected from here on.
    let mut use_startup_replacement_fees =
        force_transaction_resubmission && inbound.peek_with(|_| ()).is_some();
    loop {
        state_reporter.enter_state(L1SenderState::Idle);
        // Sleeps until at least one command is available, then greedily drains up to
        // command_limit items without waiting. cmd_buffer is emptied every iteration.
        // SYSCOIN: execute appends to MessageRoot sequentially, so tx N+1
        // cannot be prepared before tx N is mined. Keep commit/prove pipelining intact.
        let command_limit = if Input::COMPONENT_ID == ComponentId::L1SenderExecute {
            1
        } else {
            config.command_limit
        };
        let received = inbound.recv_many(&mut cmd_buffer, command_limit).await;
        // Only returns 0 when the channel is closed and drained.
        if received == 0 {
            tracing::info!("inbound channel closed");
            return Ok(());
        }
        let last = cmd_buffer
            .last()
            .context("recv_many returned non-zero count but cmd_buffer is empty")?;
        state_reporter.record_picked(
            last.last_block_number(),
            last.block_timestamp(),
            Some(last.last_batch_number()),
        );
        let use_replacement_fee_params_for_commands = use_startup_replacement_fees;
        // Keep replacement pricing while draining the immediate startup queue, but do not turn
        // `force_transaction_resubmission` into a permanent fee mode for later L1 traffic.
        if use_startup_replacement_fees && inbound.peek_with(|_| ()).is_none() {
            use_startup_replacement_fees = false;
        }

        let mut commands = cmd_buffer
            .drain(..)
            .map(|cmd| -> anyhow::Result<Input> {
                match cmd {
                    L1SenderCommand::SendToL1(command) => Ok(command),
                    L1SenderCommand::Passthrough(batch) => anyhow::bail!(
                        "Unexpected passthrough command for batch {:?}. \
                    No passthrough commands are expected after the first `SendToL1`.",
                        batch.batch_number()
                    ),
                }
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        state_reporter.enter_state(L1SenderState::SendingToL1);
        let range = Input::display_range(&commands); // Only for logging
        tracing::info!(command_name, range, "sending L1 transactions");
        L1_SENDER_METRICS.parallel_transactions[&command_name].set(commands.len() as u64);
        let sim_fee_params = resolve_fee_params(
            &provider,
            config.fee_config,
            use_replacement_fee_params_for_commands,
        )
        .await?;
        let gas_limits = estimate_gas_limits(
            &provider,
            to_address,
            gateway,
            &commands,
            operator_address,
            sim_fee_params,
        )
        .await?;
        tracing::info!(
            command_name,
            range,
            ?gas_limits,
            "estimated gas limits via eth_simulateV1",
        );
        // It's important to preserve the order of commands -
        // so that we send them downstream also in order.
        // This holds true because l1 transactions are included in the order of sender nonce.
        // Keep this in mind if changing sending logic (that is, if adding `buffer` we'd need to set nonce manually)
        // SYSCOIN: sign locally while preserving Alloy's nonce-reservation invariant for the
        // drained batch. A single pending nonce read seeds a local cursor, then each command gets
        // the next nonce before any receipt is awaited.
        let mut next_tx_nonce = provider
            .get_transaction_count(operator_address)
            .pending()
            .await
            .context("get pending operator nonce before signing L1 transaction batch")?;
        let mut pending_txs = Vec::with_capacity(commands.len());
        for (mut cmd, gas_limit) in commands.drain(..).zip(gas_limits) {
            let tx_nonce = next_tx_nonce;
            next_tx_nonce = next_tx_nonce
                .checked_add(1)
                .context("operator L1 nonce overflow while signing transaction batch")?;
            let (receipt_fut, submitted_at, raw_tx, tx_hash, tx_nonce, submitted_l1_block) =
                submit_l1_transaction(
                    &provider,
                    operator_address,
                    to_address,
                    &config,
                    gateway,
                    command_name,
                    &mut cmd,
                    &commit_submitted_tx,
                    Some(tx_nonce),
                    use_replacement_fee_params_for_commands,
                    Some(gas_limit),
                )
                .await?;
            pending_txs.push((
                receipt_fut,
                cmd,
                submitted_at,
                raw_tx,
                tx_hash,
                tx_nonce,
                submitted_l1_block,
            ));
        }
        tracing::info!(command_name, range, "sent to L1, waiting for inclusion");
        wait_for_txs_and_forward(
            pending_txs,
            &provider,
            operator_address,
            to_address,
            &config,
            gateway,
            &commit_submitted_tx,
            command_name,
            &state_reporter,
            &outbound,
        )
        .await?;
    }
}

// SYSCOIN: common L1 tx submission path used by the normal loop and by dropped-tx recovery.
async fn submit_l1_transaction<Input>(
    provider: &EthDynProvider,
    operator_address: Address,
    to_address: Address,
    config: &L1SenderConfig<Input>,
    gateway: bool,
    command_name: &'static str,
    cmd: &mut Input,
    commit_submitted_tx: &Option<watch::Sender<u64>>,
    nonce_override: Option<u64>,
    use_replacement_fee_params: bool,
    gas_limit_override: Option<u64>,
) -> anyhow::Result<(
    TransactionReceiptFuture,
    Instant,
    Option<Vec<u8>>,
    B256,
    u64,
    u64,
)>
where
    Input: SendToL1,
{
    let tx_range = Input::display_range(std::slice::from_ref(cmd));
    let fee_params =
        resolve_fee_params(provider, config.fee_config, use_replacement_fee_params).await?;
    let mut tx_request = tx_request_with_gas_fields(operator_address, fee_params)
        .with_to(to_address)
        .with_input(cmd.solidity_call(gateway, &operator_address));
    // SYSCOIN: callers pin the nonce for local signing. Normal batches allocate a monotonic
    // cursor; recovery resubmissions reuse the original nonce.
    if let Some(nonce) = nonce_override {
        tx_request.set_nonce(nonce);
    }

    let mut blob_gas_limit = 0;
    if let Some(blob_sidecar) = cmd.blob_sidecar() {
        blob_gas_limit = blob_sidecar.blobs.len() as u64 * DATA_GAS_PER_BLOB;
        let fee_per_blob_gas = provider.get_blob_base_fee().await?;
        L1_SENDER_METRICS.report_blob_base_fee(fee_per_blob_gas)?;
        let max_fee_per_blob_gas = fee_params.max_fee_per_blob_gas;

        if fee_per_blob_gas > max_fee_per_blob_gas {
            tracing::warn!(
                max_fee_per_blob_gas,
                fee_per_blob_gas,
                "L1 sender's configured maxFeePerBlobGas is lower than the one estimated from network"
            );
        }
        tx_request.set_max_fee_per_blob_gas(max_fee_per_blob_gas);

        let pending_block = provider
            .get_block(BlockId::pending())
            .await?
            .expect("no pending block");
        // todo: make conversion unconditional (and remove respective config) once anvil
        //       supports EIP-7594 blobs (see https://github.com/foundry-rs/foundry/issues/12222)
        let blob_sidecar = if config.fusaka_upgrade_timestamp <= pending_block.header.timestamp {
            BlobTransactionSidecarVariant::Eip7594(
                blob_sidecar.try_into_7594(EnvKzgSettings::Default.get())?,
            )
        } else {
            BlobTransactionSidecarVariant::Eip4844(blob_sidecar)
        };
        tx_request.set_blob_sidecar(blob_sidecar);
    };

    if let Some(gas_limit) = gas_limit_override {
        tx_request.set_gas_limit(gas_limit);
    } else {
        // SYSCOIN: recovery resubmissions are outside the normal pre-simulated batch,
        // so keep the existing padded `eth_estimateGas` path for those one-off txs.
        apply_l1_gas_limit(provider, &mut tx_request).await?;
    }

    let execution_balance_required = tx_request.max_fee_per_gas.unwrap_or_default()
        * u128::from(tx_request.gas.unwrap_or_default());
    let blob_balance_required =
        tx_request.max_fee_per_blob_gas.unwrap_or_default() * u128::from(blob_gas_limit);
    let balance_required = execution_balance_required
        .saturating_add(blob_balance_required)
        .min(u128::from(u64::MAX)) as u64;

    L1_SENDER_METRICS.balance_required_for_tx[&Input::COMPONENT_ID.as_str()].set(balance_required);

    // SYSCOIN: sign explicitly so dropped-tx recovery can rebroadcast the exact same bytes.
    let (raw_tx, tx_nonce) = sign_l1_transaction(provider, operator_address, tx_request).await?;
    let admission_retry_started = Instant::now();
    let (pending_tx, submitted_l1_block) = loop {
        let submission_baseline_block = provider.get_block_number().await?;
        match provider.send_raw_transaction(&raw_tx).await {
            Ok(pending_tx) => break (pending_tx, submission_baseline_block),
            Err(err)
                if gateway
                    && Input::COMPONENT_ID == ComponentId::L1SenderCommit
                    && is_gateway_da_admission_error(&err) =>
            {
                if admission_retry_started.elapsed() >= config.gateway_da_admission_retry_timeout {
                    return Err(anyhow::anyhow!(
                        "{command_name}: Gateway compact Bitcoin DA admission failed for {tx_range} within {:?}: {err}",
                        config.gateway_da_admission_retry_timeout
                    ));
                }
                tracing::warn!(
                    command_name,
                    tx_range,
                    error = %err,
                    elapsed = ?admission_retry_started.elapsed(),
                    retry_in = ?config.gateway_da_admission_retry_interval,
                    "Gateway rejected commit because Bitcoin DA is not visible yet; retrying submission"
                );
                tokio::time::sleep(config.gateway_da_admission_retry_interval).await;
            }
            Err(err) => return Err(err.into()),
        }
    };
    let submitted_at = Instant::now();
    let tx_hash = *pending_tx.tx_hash();
    let receipt_fut = wait_for_confirmed_receipt(
        provider.root().clone(),
        tx_hash,
        if gateway {
            REQUIRED_CONFIRMATIONS_GATEWAY
        } else {
            REQUIRED_CONFIRMATIONS_L1
        },
        config.transaction_timeout,
        config.tx_liveness_poll_interval,
        config.tx_liveness_max_missing_polls,
    )
    .boxed();
    tracing::info!(
        "{command_name}: L1 transaction submitted for {tx_range}. Hash: {tx_hash:?} Waiting for inclusion...",
    );

    // Notify CommitWatcher: this batch number has been submitted to L1.
    notify_commit_submitted(commit_submitted_tx, cmd);

    cmd.as_mut()
        .iter_mut()
        .for_each(|envelope| envelope.set_stage(Input::SENT_STAGE));

    // SYSCOIN: retain raw signed tx bytes for safe same-hash rebroadcast when the provider
    // reports the transaction as dropped before a receipt appears.
    Ok((
        receipt_fut,
        submitted_at,
        Some(raw_tx),
        tx_hash,
        tx_nonce,
        submitted_l1_block,
    ))
}

// SYSCOIN: Alloy's wallet filler signs requests into envelopes before
// `Provider::sign_transaction()` reaches the RPC-signing layer. Build and sign with the
// local wallet directly after filling the request fields required for the selected tx type.
async fn sign_l1_transaction(
    provider: &EthDynProvider,
    operator_address: Address,
    mut tx_request: TransactionRequest,
) -> anyhow::Result<(Vec<u8>, u64)> {
    if tx_request.chain_id().is_none() {
        tx_request.set_chain_id(provider.get_chain_id().await?);
    }
    if tx_request.nonce.is_none() {
        tx_request.set_nonce(
            provider
                .get_transaction_count(operator_address)
                .pending()
                .await?,
        );
    }

    let tx = tx_request
        .build(provider.wallet())
        .await
        .context("failed to sign L1 transaction with local wallet")?;
    let tx_nonce = tx.nonce();
    Ok((tx.encoded_2718(), tx_nonce))
}

// SYSCOIN: keep the commit watcher marker update identical for freshly submitted, resubmitted,
// and recovered in-flight commit transactions. Non-commit senders pass `None`.
fn notify_commit_submitted<Input: SendToL1>(
    commit_submitted_tx: &Option<watch::Sender<u64>>,
    cmd: &Input,
) {
    if commit_submitted_tx.is_some() {
        let batch_number = cmd
            .as_ref()
            .last()
            .expect("commands is non-empty when notifying commit watcher")
            .batch_number();
        notify_commit_submitted_batch(commit_submitted_tx, batch_number);
    }
}

// SYSCOIN: monotonic marker update used by `notify_commit_submitted` and unit tests.
fn notify_commit_submitted_batch(
    commit_submitted_tx: &Option<watch::Sender<u64>>,
    batch_number: u64,
) {
    if let Some(sender) = commit_submitted_tx {
        sender.send_if_modified(|current| {
            if batch_number > *current {
                *current = batch_number;
                true
            } else {
                false
            }
        });
    }
}

/// Waits for all pending L1 transaction receipts, validates them, logs balance/nonce
/// metrics, and forwards the completed commands downstream.
async fn wait_for_txs_and_forward<Input>(
    pending_txs: Vec<PendingTx<Input>>,
    provider: &EthDynProvider,
    operator_address: Address,
    to_address: Address,
    config: &L1SenderConfig<Input>,
    gateway: bool,
    commit_submitted_tx: &Option<watch::Sender<u64>>,
    command_name: &'static str,
    state_reporter: &ComponentStateReporter,
    outbound: &mpsc::Sender<SignedBatchEnvelope<FriProof>>,
) -> anyhow::Result<()>
where
    Input: SendToL1,
{
    state_reporter.enter_state(L1SenderState::WaitingL1Inclusion);

    let required_confirmations = if gateway {
        REQUIRED_CONFIRMATIONS_GATEWAY
    } else {
        REQUIRED_CONFIRMATIONS_L1
    };
    let transaction_timeout = config.transaction_timeout;
    let mut completed_commands = Vec::with_capacity(pending_txs.len());
    for (
        mut receipt_fut,
        mut command,
        mut submitted_at,
        mut raw_tx,
        mut tx_hash,
        mut tx_nonce,
        mut submitted_l1_block,
    ) in pending_txs
    {
        let receipt = loop {
            let receipt = receipt_fut.await;
            // Observe latency before propagating errors so provider/RPC failures are recorded.
            L1_SENDER_METRICS.tx_inclusion_latency_seconds[&command_name]
                .observe(submitted_at.elapsed().as_secs_f64());
            match receipt? {
                ReceiptWaitOutcome::Confirmed(receipt) => break receipt,
                ReceiptWaitOutcome::TimedOut => {
                    tracing::warn!(
                        command_name,
                        ?tx_hash,
                        tx_nonce,
                        "L1 transaction is still pending after timeout; resubmitting at the \
                         same nonce with replacement fee params"
                    );
                    match submit_l1_transaction(
                        provider,
                        operator_address,
                        to_address,
                        config,
                        gateway,
                        command_name,
                        &mut command,
                        commit_submitted_tx,
                        Some(tx_nonce),
                        true,
                        None,
                    )
                    .await
                    {
                        Ok(resubmitted) => {
                            receipt_fut = resubmitted.0;
                            submitted_at = resubmitted.1;
                            raw_tx = resubmitted.2;
                            tx_hash = resubmitted.3;
                            tx_nonce = resubmitted.4;
                            submitted_l1_block = resubmitted.5;
                            continue;
                        }
                        Err(err) => {
                            if let Some(transport_err) = err.downcast_ref::<TransportError>()
                                && is_nonce_reuse_rebroadcast_error(transport_err)
                            {
                                tx_hash = recover_same_nonce_tx(
                                    provider,
                                    operator_address,
                                    to_address,
                                    tx_nonce,
                                    tx_hash,
                                    submitted_l1_block,
                                    gateway,
                                    command_name,
                                    &command,
                                    transaction_timeout,
                                    transport_err,
                                )
                                .await?;
                                raw_tx = None;
                                tracing::warn!(
                                    command_name,
                                    ?tx_hash,
                                    tx_nonce,
                                    "Tracking matching L1 transaction found at timed-out nonce"
                                );
                                receipt_fut = wait_for_confirmed_receipt(
                                    provider.root().clone(),
                                    tx_hash,
                                    required_confirmations,
                                    transaction_timeout,
                                    config.tx_liveness_poll_interval,
                                    config.tx_liveness_max_missing_polls,
                                )
                                .boxed();
                                submitted_at = Instant::now();
                                continue;
                            }
                            if let Some(transport_err) = err.downcast_ref::<TransportError>()
                                && is_benign_rebroadcast_error(transport_err)
                            {
                                tracing::warn!(
                                    command_name,
                                    ?tx_hash,
                                    tx_nonce,
                                    "Timed-out L1 transaction replacement returned a benign error; \
                                     continuing to wait: {transport_err}",
                                );
                                receipt_fut = wait_for_confirmed_receipt(
                                    provider.root().clone(),
                                    tx_hash,
                                    required_confirmations,
                                    transaction_timeout,
                                    config.tx_liveness_poll_interval,
                                    config.tx_liveness_max_missing_polls,
                                )
                                .boxed();
                                submitted_at = Instant::now();
                                continue;
                            }
                            return Err(err);
                        }
                    }
                }
                // SYSCOIN: timeout expiry is non-fatal. A dropped tx is recovered by rebroadcasting
                // the same raw payload when available, or by resubmitting the original command for
                // recovered startup txs where raw bytes are unavailable.
                ReceiptWaitOutcome::Dropped => {
                    let Some(raw_tx_bytes) = raw_tx.as_ref() else {
                        tracing::warn!(
                            command_name,
                            ?tx_hash,
                            "Recovered L1 transaction is no longer visible; resubmitting command"
                        );
                        let resubmitted = submit_l1_transaction(
                            provider,
                            operator_address,
                            to_address,
                            config,
                            gateway,
                            command_name,
                            &mut command,
                            commit_submitted_tx,
                            Some(tx_nonce),
                            true,
                            None,
                        )
                        .await?;
                        receipt_fut = resubmitted.0;
                        submitted_at = resubmitted.1;
                        raw_tx = resubmitted.2;
                        tx_hash = resubmitted.3;
                        tx_nonce = resubmitted.4;
                        submitted_l1_block = resubmitted.5;
                        continue;
                    };

                    // SYSCOIN: if the provider no longer sees an unmined transaction by hash,
                    // rebroadcast the exact signed payload. This avoids crashing or advancing
                    // the nonce chain while giving dropped transactions a recovery path.
                    match provider.send_raw_transaction(raw_tx_bytes).await {
                        Ok(pending_tx) => {
                            tx_hash = *pending_tx.tx_hash();
                            tracing::warn!(
                                command_name,
                                ?tx_hash,
                                "Rebroadcast dropped L1 transaction; waiting for inclusion"
                            );
                        }
                        Err(err) => {
                            if is_benign_rebroadcast_error(&err) {
                                tracing::warn!(
                                    command_name,
                                    ?tx_hash,
                                    "L1 transaction rebroadcast returned a benign error; continuing to wait: {err}",
                                );
                            } else if is_nonce_reuse_rebroadcast_error(&err) {
                                tx_hash = recover_same_nonce_tx(
                                    provider,
                                    operator_address,
                                    to_address,
                                    tx_nonce,
                                    tx_hash,
                                    submitted_l1_block,
                                    gateway,
                                    command_name,
                                    &command,
                                    transaction_timeout,
                                    &err,
                                )
                                .await?;
                                raw_tx = None;
                                tracing::warn!(
                                    command_name,
                                    ?tx_hash,
                                    tx_nonce,
                                    "Tracking matching L1 transaction found at reused nonce"
                                );
                            } else {
                                tracing::warn!(
                                    command_name,
                                    ?tx_hash,
                                    "Failed to rebroadcast L1 transaction; resubmitting command: {err}",
                                );
                                let resubmitted = submit_l1_transaction(
                                    provider,
                                    operator_address,
                                    to_address,
                                    config,
                                    gateway,
                                    command_name,
                                    &mut command,
                                    commit_submitted_tx,
                                    Some(tx_nonce),
                                    true,
                                    None,
                                )
                                .await?;
                                receipt_fut = resubmitted.0;
                                submitted_at = resubmitted.1;
                                raw_tx = resubmitted.2;
                                tx_hash = resubmitted.3;
                                tx_nonce = resubmitted.4;
                                submitted_l1_block = resubmitted.5;
                                continue;
                            }
                        }
                    }
                    receipt_fut = wait_for_confirmed_receipt(
                        provider.root().clone(),
                        tx_hash,
                        required_confirmations,
                        transaction_timeout,
                        config.tx_liveness_poll_interval,
                        config.tx_liveness_max_missing_polls,
                    )
                    .boxed();
                    submitted_at = Instant::now();
                }
            }
        };
        validate_tx_receipt(provider, &command, receipt).await?;
        completed_commands.push(command);
    }

    let range = Input::display_range(&completed_commands);
    let balance = format_ether(provider.get_balance(operator_address).await?);
    let nonce = provider.get_transaction_count(operator_address).await?;
    tracing::info!(
        command_name,
        range,
        balance,
        nonce,
        "all transactions included, sending downstream",
    );
    L1_SENDER_METRICS.balance[&command_name].set(balance.parse()?);
    L1_SENDER_METRICS.nonce[&command_name].set(nonce);

    for command in completed_commands {
        for mut output_envelope in command.into() {
            output_envelope.set_stage(Input::MINED_STAGE);
            outbound.send_and_record(output_envelope, state_reporter)?;
        }
    }
    Ok(())
}

// SYSCOIN: nonce-reuse rebroadcast errors mean the original nonce may already be occupied.
// Keep looking for the same-nonce tx instead of resubmitting the command at a later nonce or
// re-arming a waiter for the dropped hash.
async fn recover_same_nonce_tx<Input>(
    provider: &EthDynProvider,
    operator_address: Address,
    to_address: Address,
    nonce: u64,
    old_tx_hash: B256,
    submitted_l1_block: u64,
    gateway: bool,
    command_name: &'static str,
    command: &Input,
    timeout: std::time::Duration,
    rebroadcast_err: &TransportError,
) -> anyhow::Result<B256>
where
    Input: SendToL1,
{
    let started_at = Instant::now();
    let poll_interval = provider.client().poll_interval();
    let mut logged_unsupported_rpc = false;
    let mut next_warning_at = if timeout.is_zero() {
        None
    } else {
        Some(timeout)
    };

    loop {
        match find_matching_sender_nonce_tx(
            provider,
            operator_address,
            to_address,
            nonce,
            submitted_l1_block,
            gateway,
            command_name,
            command,
        )
        .await?
        {
            SameNonceTx::Found(tx_hash) => return Ok(tx_hash),
            SameNonceTx::NotFound => {
                let elapsed = started_at.elapsed();
                if !timeout.is_zero() && elapsed >= timeout {
                    anyhow::bail!(
                        "L1 transaction rebroadcast returned a nonce-reuse error for \
                         {command_name} tx {old_tx_hash:?} at nonce {nonce}, but no matching \
                         same-nonce transaction became visible within {timeout:?}: {rebroadcast_err}"
                    );
                }
                if let Some(warning_at) = next_warning_at
                    && elapsed >= warning_at
                {
                    tracing::warn!(
                        command_name,
                        ?old_tx_hash,
                        nonce,
                        waited_secs = elapsed.as_secs_f64(),
                        "L1 transaction rebroadcast returned a nonce-reuse error, but no matching \
                         same-nonce transaction is visible yet; retrying discovery: {rebroadcast_err}",
                    );
                    next_warning_at = Some(warning_at + timeout);
                }
                tokio::time::sleep(poll_interval).await;
            }
            SameNonceTx::Unsupported => {
                let elapsed = started_at.elapsed();
                if !timeout.is_zero() && elapsed >= timeout {
                    anyhow::bail!(
                        "L1 transaction rebroadcast returned a nonce-reuse error for \
                         {command_name} tx {old_tx_hash:?} at nonce {nonce}, but \
                         eth_getTransactionBySenderAndNonce is unsupported and standard \
                         block-scan recovery found no matching tx within {timeout:?}: {rebroadcast_err}"
                    );
                }
                let should_log = if let Some(warning_at) = next_warning_at
                    && elapsed >= warning_at
                {
                    next_warning_at = Some(warning_at + timeout);
                    true
                } else if timeout.is_zero() && !logged_unsupported_rpc {
                    logged_unsupported_rpc = true;
                    true
                } else {
                    false
                };
                if should_log {
                    tracing::warn!(
                        command_name,
                        ?old_tx_hash,
                        nonce,
                        first_block = submitted_l1_block,
                        waited_secs = elapsed.as_secs_f64(),
                        "L1 transaction rebroadcast returned a nonce-reuse error and \
                         eth_getTransactionBySenderAndNonce is unsupported; retrying standard \
                         block-scan recovery: {rebroadcast_err}",
                    );
                }
                tokio::time::sleep(poll_interval).await;
            }
        }
    }
}

// SYSCOIN: standard-RPC fallback for providers that do not implement sender+nonce lookup.
// Scan recent mined blocks and accept only a transaction with the same sender, nonce, and calldata.
async fn find_matching_mined_sender_nonce_tx<Input>(
    provider: &EthDynProvider,
    operator_address: Address,
    to_address: Address,
    nonce: u64,
    first_block: u64,
    gateway: bool,
    command_name: &'static str,
    command: &Input,
) -> anyhow::Result<Option<B256>>
where
    Input: SendToL1,
{
    let latest_block = provider.get_block_number().await?;
    let expected_input = command.solidity_call(gateway, &operator_address);

    for block_number in (first_block..=latest_block).rev() {
        let Some(block) = provider
            .get_block(BlockId::number(block_number))
            .full()
            .await?
        else {
            continue;
        };

        for tx in block.transactions().txns() {
            if tx.from() != operator_address || tx.nonce() != nonce {
                continue;
            }
            if tx.to() != Some(to_address) {
                anyhow::bail!(
                    "Mined same-nonce L1 transaction for {command_name} at nonce {nonce} \
                     targets a different address"
                );
            }
            if tx.input().as_ref() != expected_input.as_ref() {
                anyhow::bail!(
                    "Mined same-nonce L1 transaction for {command_name} at nonce {nonce} \
                     has different calldata"
                );
            }
            return Ok(Some(tx.tx_hash()));
        }
    }

    Ok(None)
}

// SYSCOIN: only errors that indicate the exact raw tx is still known are benign. Keep the
// `known transaction` match anchored so messages like `unknown transaction type` are not benign.
fn is_benign_rebroadcast_error(err: &TransportError) -> bool {
    match err {
        TransportError::ErrorResp(resp) => {
            let message = resp.message.to_ascii_lowercase();
            message.contains("already known")
                || message.contains("already imported")
                || message.trim_start().starts_with("known transaction")
        }
        _ => false,
    }
}

// SYSCOIN: nonce-reuse errors are ambiguous. The tx may already be accepted/mined by a different
// backend, or the nonce may be occupied by a replacement. Do not blindly resubmit at a later nonce.
fn is_nonce_reuse_rebroadcast_error(err: &TransportError) -> bool {
    match err {
        TransportError::ErrorResp(resp) => {
            let message = resp.message.to_ascii_lowercase();
            message.contains("nonce too low")
                || message.contains("replacement transaction underpriced")
        }
        _ => false,
    }
}

// SYSCOIN: Gateway performs compact edge-DA admission before mempool insertion. A child chain can
// publish Bitcoin DA through its local Syscoin node while the Gateway node has not observed the DA
// yet, so this specific pre-send rejection is transient and must be retried by the child chain.
fn is_gateway_da_admission_error(err: &TransportError) -> bool {
    match err {
        TransportError::ErrorResp(resp) => {
            let message = resp.message.to_ascii_lowercase();
            is_retryable_gateway_da_admission_message(&message)
        }
        _ => false,
    }
}

fn is_retryable_gateway_da_admission_message(message: &str) -> bool {
    message.contains("not retrievable")
        && (message.contains("compact edge da ref") || message.contains("bitcoin da"))
}

// SYSCOIN: outcome of same-nonce discovery after a nonce-reuse rebroadcast error.
enum SameNonceTx {
    Found(B256),
    NotFound,
    Unsupported,
}

// SYSCOIN: if a rebroadcast reports nonce reuse, try to discover the tx currently occupying the
// original sender nonce and track it only if it carries the same command calldata.
async fn find_matching_sender_nonce_tx<Input>(
    provider: &EthDynProvider,
    operator_address: Address,
    to_address: Address,
    nonce: u64,
    submitted_l1_block: u64,
    gateway: bool,
    command_name: &'static str,
    command: &Input,
) -> anyhow::Result<SameNonceTx>
where
    Input: SendToL1,
{
    let tx = match provider
        .get_transaction_by_sender_nonce(operator_address, nonce)
        .await
    {
        Ok(tx) => tx,
        Err(TransportError::ErrorResp(ref e)) if e.code == METHOD_NOT_FOUND_CODE => {
            return find_matching_mined_sender_nonce_tx(
                provider,
                operator_address,
                to_address,
                nonce,
                submitted_l1_block,
                gateway,
                command_name,
                command,
            )
            .await
            .map(|tx_hash| tx_hash.map_or(SameNonceTx::Unsupported, SameNonceTx::Found));
        }
        Err(err) => anyhow::bail!(
            "Failed to fetch same-nonce L1 transaction for {command_name} at nonce {nonce}: {err}"
        ),
    };

    let Some(tx) = tx else {
        return Ok(SameNonceTx::NotFound);
    };

    if tx.to() != Some(to_address) {
        anyhow::bail!(
            "Same-nonce L1 transaction for {command_name} at nonce {nonce} targets a different address"
        );
    }

    let expected_input = command.solidity_call(gateway, &operator_address);
    if tx.input().as_ref() != expected_input.as_ref() {
        anyhow::bail!(
            "Same-nonce L1 transaction for {command_name} at nonce {nonce} has different calldata"
        );
    }

    Ok(SameNonceTx::Found(tx.tx_hash()))
}

async fn wait_for_confirmed_receipt<P>(
    provider: P,
    tx_hash: B256,
    required_confirmations: u64,
    timeout: std::time::Duration,
    tx_liveness_poll_interval: std::time::Duration,
    tx_liveness_max_missing_polls: u32,
) -> anyhow::Result<ReceiptWaitOutcome>
where
    P: Provider,
{
    let started_at = Instant::now();
    let poll_interval = provider.client().poll_interval();
    let liveness_enabled =
        tx_liveness_max_missing_polls > 0 && !tx_liveness_poll_interval.is_zero();
    let mut next_liveness_poll_at = liveness_enabled.then_some(tx_liveness_poll_interval);
    let mut consecutive_missing_polls: u32 = 0;
    let mut next_warning_at = if timeout.is_zero() {
        None
    } else {
        Some(timeout)
    };

    loop {
        let latest_block = provider.get_block_number().await.map_err(|err| {
            tracing::warn!(
                "Failed to fetch latest L1 block while waiting for transaction confirmation \
                 for tx {tx_hash}: {err}",
            );
            anyhow::Error::from(err)
        })?;
        let receipt = match provider.get_transaction_receipt(tx_hash).await {
            Ok(receipt) => receipt,
            Err(err) => {
                tracing::warn!(
                    "Failed to fetch transaction receipt while waiting for confirmation \
                     for tx {tx_hash}: {err}",
                );
                return Err(err.into());
            }
        };
        if let Some(receipt) = receipt.as_ref() {
            let receipt_block_number = receipt
                .block_number
                .context("transaction receipt missing block number")?;
            let confirmed_at =
                receipt_block_number.saturating_add(required_confirmations.saturating_sub(1));
            if latest_block >= confirmed_at {
                return Ok(ReceiptWaitOutcome::Confirmed(receipt.clone()));
            }
            consecutive_missing_polls = 0;
        }

        let elapsed = started_at.elapsed();
        let receipt_block_number = receipt.as_ref().and_then(|receipt| receipt.block_number);
        let confirmed_at =
            receipt_block_number.map(|block| block + required_confirmations.saturating_sub(1));
        // SYSCOIN: check dropped-tx liveness on a shorter cadence than the warning interval.
        // Missing consecutive by-hash polls mean the tx was accepted then purged/rejected, so the
        // caller should recover instead of stalling until `transaction_timeout`.
        if receipt.is_none()
            && let Some(liveness_poll_at) = next_liveness_poll_at
            && elapsed >= liveness_poll_at
        {
            match provider.get_transaction_by_hash(tx_hash).await {
                Ok(None) => {
                    consecutive_missing_polls = consecutive_missing_polls.saturating_add(1);
                    tracing::warn!(
                        ?tx_hash,
                        consecutive_missing_polls,
                        tx_liveness_max_missing_polls,
                        "L1 transaction is not visible by hash while waiting for confirmation"
                    );
                    if consecutive_missing_polls >= tx_liveness_max_missing_polls {
                        tracing::warn!(
                            ?tx_hash,
                            consecutive_missing_polls,
                            "L1 transaction stayed missing by hash; treating it as dropped"
                        );
                        return Ok(ReceiptWaitOutcome::Dropped);
                    }
                }
                Ok(Some(_)) => {
                    consecutive_missing_polls = 0;
                }
                Err(err) => {
                    tracing::warn!(
                        "Failed to check whether L1 transaction {tx_hash} is still visible \
                         while waiting for confirmation: {err}"
                    );
                }
            }
            next_liveness_poll_at = Some(started_at.elapsed() + tx_liveness_poll_interval);
        }

        // SYSCOIN: if the nonce-bearing transaction stays pending past the configured timeout,
        // ask the sender loop to replace it at the same nonce using replacement fee params.
        if let Some(warning_at) = next_warning_at
            && elapsed >= warning_at
        {
            tracing::warn!(
                "Timed out waiting for L1 transaction confirmation for tx {tx_hash}. \
                 required_confirmations={required_confirmations}, \
                 waited_secs={}, latest_l1_block={latest_block}, \
                 receipt_block_number={receipt_block_number:?}, confirmed_at={confirmed_at:?}",
                elapsed.as_secs_f64(),
            );
            if receipt.is_none() {
                return Ok(ReceiptWaitOutcome::TimedOut);
            }
            next_warning_at = Some(warning_at + timeout);
        }

        tokio::time::sleep(poll_interval).await;
    }
}

/// Detects in-flight L1 transactions from a previous session, pairs each one with the
/// corresponding queued command, and returns them ready to hand to the main loop.
///
/// For each in-flight tx, the next command is peeked and its calldata is compared against
/// the on-chain input. On a match the command is consumed and paired. On the first mismatch
/// the loop stops and whatever has been paired so far is returned — the unmatched command
/// remains in `inbound` for the normal send path.
///
/// `sl_block_number` must be the same L1 block at which `getTotalBatches*` was called when
/// constructing the inbound command queue. Pinning the confirmed-nonce baseline to that block
/// prevents the race where txs mined between the `getTotalBatches` call and this nonce check
/// cause us to mis-count in-flight txs and crash on calldata mismatch.
async fn recover_in_flight_txs<Input>(
    provider: &EthDynProvider,
    operator_address: Address,
    gateway: bool,
    inbound: &mut PeekableReceiver<L1SenderCommand<Input>>,
    command_name: &str,
    sl_block_number: u64,
    state_reporter: &ComponentStateReporter,
) -> anyhow::Result<Vec<(alloy::primitives::B256, Input, u64)>>
where
    Input: SendToL1 + Send + 'static,
{
    let latest_nonce = provider
        .get_transaction_count(operator_address)
        .block_id(BlockId::number(sl_block_number))
        .await
        .context("get confirmed transaction count")?;
    let pending_nonce = provider
        .get_transaction_count(operator_address)
        .pending()
        .await
        .context("get pending transaction count")?;

    if pending_nonce <= latest_nonce {
        return Ok(vec![]);
    }

    let in_flight_count = (pending_nonce - latest_nonce) as usize;
    tracing::info!(
        command_name,
        sl_block_number,
        latest_nonce,
        pending_nonce,
        in_flight_count,
        "Detected in-flight L1 transactions on startup, attempting recovery",
    );

    // Probe whether the provider supports `eth_getTransactionBySenderAndNonce` before
    // iterating over all pending nonces.
    if let Err(TransportError::ErrorResp(ref e)) = provider
        .get_transaction_by_sender_nonce(operator_address, latest_nonce)
        .await
    {
        if e.code == METHOD_NOT_FOUND_CODE {
            tracing::warn!(
                command_name,
                "eth_getTransactionBySenderAndNonce is not supported by current provider.",
            );
            return Ok(vec![]);
        }
        anyhow::bail!("Error while probing eth_getTransactionBySenderAndNonce support: {e}");
    }

    // For each pending nonce, fetch the in-flight tx then peek at the next queued command.
    // If the command's calldata matches what is on-chain, consume and pair it. On the first
    // mismatch, stop — the unmatched command stays in `inbound` and will be re-sent by the
    // normal send path (replacing the in-flight tx at that nonce).
    let mut paired = Vec::with_capacity(in_flight_count);
    for nonce in latest_nonce..pending_nonce {
        let tx = match provider
            .get_transaction_by_sender_nonce(operator_address, nonce)
            .await
        {
            Err(err) => {
                anyhow::bail!("Failed to fetch in-flight transaction at nonce {nonce}: {err}");
            }
            Ok(Some(tx)) => tx,
            Ok(None) => {
                tracing::warn!(
                    command_name,
                    nonce,
                    "In-flight transaction at nonce {nonce} was dropped from the mempool.",
                );
                return Ok(paired);
            }
        };

        // Peek at the next command without consuming it so that a mismatch leaves
        // `inbound` intact for the normal send path.
        let matches = inbound
            .peek_recv(|raw_cmd| {
                let L1SenderCommand::SendToL1(cmd) = raw_cmd else {
                    return false;
                };
                cmd.solidity_call(gateway, &operator_address) == *tx.input()
            })
            .await;

        match matches {
            None => anyhow::bail!("inbound channel closed during in-flight recovery"),
            Some(false) => {
                tracing::warn!(
                    command_name,
                    nonce,
                    "In-flight transaction calldata does not match the next queued command. \
                     Stopping recovery at nonce {nonce}.",
                );
                break;
            }
            Some(true) => {
                let Some(L1SenderCommand::SendToL1(cmd)) =
                    inbound.recv_and_record_picked(state_reporter).await
                else {
                    unreachable!("peek succeeded, recv must return the same item");
                };
                paired.push((tx.tx_hash(), cmd, nonce));
            }
        }
    }

    tracing::info!(
        command_name,
        recovered = paired.len(),
        in_flight_count,
        "Recovered in-flight transactions; will wait for their inclusion before accepting new commands",
    );

    Ok(paired)
}

async fn process_prepending_passthrough_commands<Input: SendToL1 + Send + 'static>(
    inbound: &mut PeekableReceiver<L1SenderCommand<Input>>,
    outbound: &mpsc::Sender<SignedBatchEnvelope<FriProof>>,
    state_reporter: &ComponentStateReporter,
    command_name: &str,
) -> anyhow::Result<Option<()>> {
    loop {
        state_reporter.enter_state(L1SenderState::Idle);
        match inbound
            .peek_recv(|command| matches!(command, L1SenderCommand::Passthrough(_)))
            .await
        {
            None => return Ok(None),
            // command is SendToL1 (not passthrough)
            // we don't expect anymore passthroughs and can proceed with normal operations
            Some(false) => return Ok(Some(())),
            // command is passthrough
            Some(true) => {
                let Some(next_command) = inbound.recv_and_record_picked(state_reporter).await
                else {
                    return Ok(None);
                };
                match next_command {
                    L1SenderCommand::SendToL1(_) => {
                        anyhow::bail!("Mismatch between peeked and received command")
                    }
                    L1SenderCommand::Passthrough(batch) => {
                        tracing::info!(
                            command_name,
                            batch_number = batch.batch_number(),
                            "Not actually sending to L1, just passing through"
                        );
                        outbound.send_and_record(
                            (*batch).with_stage(Input::PASSTHROUGH_STAGE),
                            state_reporter,
                        )?;
                    }
                }
            }
        }
    }
}

impl L1SenderFeeConfig {
    fn validate_syscoin_fee_caps(self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.max_fee_per_gas_wei >= SYSCOIN_L1_PRIORITY_FEE_FLOOR_WEI,
            "L1 sender's configured maxFeePerGas ({}) is below the Syscoin priority fee floor ({})",
            self.max_fee_per_gas_wei,
            SYSCOIN_L1_PRIORITY_FEE_FLOOR_WEI
        );
        anyhow::ensure!(
            self.max_priority_fee_per_gas_wei >= SYSCOIN_L1_PRIORITY_FEE_FLOOR_WEI,
            "L1 sender's configured maxPriorityFeePerGas ({}) is below the Syscoin priority fee floor ({})",
            self.max_priority_fee_per_gas_wei,
            SYSCOIN_L1_PRIORITY_FEE_FLOOR_WEI
        );
        anyhow::ensure!(
            self.max_fee_per_gas_wei >= self.max_priority_fee_per_gas_wei,
            "L1 sender's configured maxFeePerGas ({}) is below maxPriorityFeePerGas ({})",
            self.max_fee_per_gas_wei,
            self.max_priority_fee_per_gas_wei
        );

        let replacement = self.replacement_fee_params();
        anyhow::ensure!(
            replacement.max_fee_per_gas >= replacement.max_priority_fee_per_gas,
            "L1 sender's replacement maxFeePerGas ({}) is below replacement maxPriorityFeePerGas ({})",
            replacement.max_fee_per_gas,
            replacement.max_priority_fee_per_gas
        );
        Ok(())
    }

    fn configured_fee_params(self) -> FeeParams {
        FeeParams {
            max_fee_per_gas: self.max_fee_per_gas_wei,
            max_priority_fee_per_gas: self.max_priority_fee_per_gas_wei,
            max_fee_per_blob_gas: self.max_fee_per_blob_gas_wei,
        }
    }

    fn replacement_fee_params(self) -> FeeParams {
        // SYSCOIN: bump from the effective floored fee so replacement txs remain
        // strictly above the first submission even when the configured tip cap is lower.
        let base = self
            .configured_fee_params()
            .with_syscoin_priority_fee_floor();
        FeeParams {
            max_fee_per_gas: ((base.max_fee_per_gas as f64)
                * self.max_fee_per_gas_replacement_multiplier)
                .ceil() as u128,
            max_priority_fee_per_gas: ((base.max_priority_fee_per_gas as f64)
                * self.max_priority_fee_per_gas_replacement_multiplier)
                .ceil() as u128,
            max_fee_per_blob_gas: ((self.max_fee_per_blob_gas_wei as f64)
                * self.max_fee_per_blob_gas_replacement_multiplier)
                .ceil() as u128,
        }
        .with_syscoin_priority_fee_floor()
    }
}

async fn resolve_fee_params(
    provider: &dyn Provider,
    fee_config: L1SenderFeeConfig,
    use_replacement_fee_params: bool,
) -> anyhow::Result<FeeParams> {
    if use_replacement_fee_params {
        return Ok(fee_config.replacement_fee_params());
    }

    let configured_params = fee_config.configured_fee_params();
    let eip1559_est = provider.estimate_eip1559_fees().await?;
    L1_SENDER_METRICS.report_l1_eip_1559_estimation(eip1559_est)?;
    // SYSCOIN: custom priority-fee estimates are observability-only; do not block L1
    // transaction submission if a provider cannot serve the extra fee-history calls.
    if let Err(err) = report_custom_priority_fee_metrics(provider).await {
        tracing::warn!("failed to report custom priority-fee metrics: {err:#}");
    }

    tracing::debug!(
        max_priority_fee_per_gas_gwei = ?format_units(eip1559_est.max_priority_fee_per_gas, "gwei"),
        max_fee_per_gas_gwei = ?format_units(eip1559_est.max_fee_per_gas, "gwei"),
        "estimated priority and max fees"
    );

    Ok(apply_fee_caps(configured_params, eip1559_est))
}

/// Combines operator-configured fee caps with the network's EIP-1559 estimate.
///
/// `max_fee_per_gas` and `max_fee_per_blob_gas` are taken verbatim from
/// `configured` - they are static caps set by the operator and never adjusted
/// up from network estimates. Only `max_priority_fee_per_gas` follows the
/// estimate, capped from above by the configured value.
fn apply_fee_caps(configured: FeeParams, estimated: Eip1559Estimation) -> FeeParams {
    if estimated.max_fee_per_gas > configured.max_fee_per_gas {
        tracing::warn!(
            "L1 sender's configured maxFeePerGas ({}) \
             is lower than the one estimated from network  ({}), \
             using the configured base fee value ({}) - this may result in inclusion delay.",
            configured.max_fee_per_gas,
            estimated.max_fee_per_gas,
            configured.max_fee_per_gas,
        );
    }

    let max_priority_fee_per_gas =
        if estimated.max_priority_fee_per_gas > configured.max_priority_fee_per_gas {
            tracing::warn!(
                "L1 sender's configured max_priority_fee_per_gas ({}) \
             is lower than the one estimated from network  ({}), \
             using the configured priority fee value ({}) - this may result in inclusion delay.",
                configured.max_priority_fee_per_gas,
                estimated.max_priority_fee_per_gas,
                configured.max_priority_fee_per_gas,
            );
            configured.max_priority_fee_per_gas
        } else {
            estimated.max_priority_fee_per_gas
        };

    let fee_params = FeeParams {
        max_fee_per_gas: configured.max_fee_per_gas,
        max_priority_fee_per_gas,
        max_fee_per_blob_gas: configured.max_fee_per_blob_gas,
    };
    fee_params.with_syscoin_priority_fee_floor()
}

impl FeeParams {
    fn with_syscoin_priority_fee_floor(mut self) -> Self {
        if self.max_priority_fee_per_gas < SYSCOIN_L1_PRIORITY_FEE_FLOOR_WEI {
            tracing::warn!(
                max_priority_fee_per_gas = self.max_priority_fee_per_gas,
                floor = SYSCOIN_L1_PRIORITY_FEE_FLOOR_WEI,
                "Applying Syscoin L1 priority fee floor"
            );
            self.max_priority_fee_per_gas = SYSCOIN_L1_PRIORITY_FEE_FLOOR_WEI;
        }
        self
    }
}

fn tx_request_with_gas_fields(
    operator_address: Address,
    fee_params: FeeParams,
) -> TransactionRequest {
    TransactionRequest::default()
        .with_from(operator_address)
        .with_max_fee_per_gas(fee_params.max_fee_per_gas)
        .with_max_priority_fee_per_gas(fee_params.max_priority_fee_per_gas)
}

// SYSCOIN
async fn apply_l1_gas_limit(
    provider: &dyn Provider,
    tx_request: &mut TransactionRequest,
) -> anyhow::Result<()> {
    let estimated_gas = provider.estimate_gas(tx_request.clone()).await?;
    let latest_block = provider
        .get_block(BlockId::latest())
        .await?
        .context("latest L1 block is unavailable while setting L1 gas limit")?;
    let block_gas_limit = latest_block.header.gas_limit;

    if estimated_gas > block_gas_limit {
        anyhow::bail!(
            "estimated L1 transaction gas ({estimated_gas}) exceeds latest L1 block gas limit ({block_gas_limit})",
        );
    }

    let padded_gas_limit = estimated_gas
        .saturating_mul(L1_TX_GAS_ESTIMATE_PADDING_NUMERATOR)
        .div_ceil(L1_TX_GAS_ESTIMATE_PADDING_DENOMINATOR);
    let gas_limit = padded_gas_limit.min(block_gas_limit);

    if gas_limit < padded_gas_limit {
        tracing::warn!(
            estimated_gas,
            padded_gas_limit,
            block_gas_limit,
            gas_limit,
            "capping L1 transaction gas limit at latest block gas limit"
        );
    }

    tx_request.set_gas_limit(gas_limit);
    Ok(())
}

/// Estimates gas limits for a batch of L1 commands via `eth_simulateV1`, returning
/// `2 * gas_used` per call. Each command goes into its own simulated block so cumulative
/// block-gas-limit constraints cannot reject the batch, while writes from earlier blocks
/// remain visible to later ones. Falls back to [`L1_GAS_LIMIT_FALLBACK`] per tx on errors.
async fn estimate_gas_limits<Input>(
    provider: &EthDynProvider,
    to_address: Address,
    gateway: bool,
    commands: &[Input],
    operator_address: Address,
    fee_params: FeeParams,
) -> anyhow::Result<Vec<u64>>
where
    Input: SendToL1,
{
    let starting_nonce = provider
        .get_transaction_count(operator_address)
        .pending()
        .await
        .context("get pending nonce for L1 sender gas estimation")?;
    let balance_override = StateOverridesBuilder::default()
        .append(
            operator_address,
            AccountOverride {
                balance: Some(U256::MAX),
                ..Default::default()
            },
        )
        .build();
    // SYSCOIN: preserve the pre-simulate gas-limit safety invariant from
    // `apply_l1_gas_limit`: never sign an L1 tx whose gas limit exceeds the
    // currently observed L1 block gas limit, including fallback paths below.
    let latest_block = provider
        .get_block(BlockId::latest())
        .await?
        .context("latest L1 block is unavailable while setting simulated L1 gas limits")?;
    let block_gas_limit = latest_block.header.gas_limit;
    let fallback_gas_limit = L1_GAS_LIMIT_FALLBACK.min(block_gas_limit);
    if fallback_gas_limit < L1_GAS_LIMIT_FALLBACK {
        tracing::warn!(
            fallback_gas_limit = L1_GAS_LIMIT_FALLBACK,
            block_gas_limit,
            gas_limit = fallback_gas_limit,
            "capping fallback L1 transaction gas limit at latest block gas limit"
        );
    }
    const SIM_GAS_LIMIT: u64 = 30_000_000;
    let sim_gas_limit = SIM_GAS_LIMIT.min(block_gas_limit);
    if sim_gas_limit < SIM_GAS_LIMIT {
        tracing::warn!(
            configured_sim_gas_limit = SIM_GAS_LIMIT,
            block_gas_limit,
            gas_limit = sim_gas_limit,
            "capping L1 simulation transaction gas limit at latest block gas limit"
        );
    }
    let block_state_calls = commands
        .iter()
        .enumerate()
        .map(|(i, cmd)| {
            let mut req = TransactionRequest::default()
                .with_from(operator_address)
                .with_to(to_address)
                .with_input(cmd.solidity_call(gateway, &operator_address))
                .with_max_fee_per_gas(fee_params.max_fee_per_gas)
                .with_max_priority_fee_per_gas(fee_params.max_priority_fee_per_gas)
                .with_nonce(starting_nonce + i as u64)
                .with_gas_limit(sim_gas_limit);
            if let Some(sidecar) = cmd.blob_sidecar() {
                req.blob_versioned_hashes = Some(sidecar.versioned_hashes().collect());
                req.max_fee_per_blob_gas = Some(fee_params.max_fee_per_blob_gas);
                // Anvil routes blob requests through the EIP-4844 arm only when
                // `type=3` is set explicitly; otherwise it returns -32602.
                req.transaction_type = Some(3);
            }
            let mut sim_block = SimBlock::default().call(req);
            sim_block.state_overrides = Some(balance_override.clone());
            sim_block
        })
        .collect();

    let payload = SimulatePayload {
        block_state_calls,
        ..Default::default()
    };

    let blocks = match provider.simulate(&payload).pending().await {
        Ok(blocks) if blocks.len() == commands.len() => blocks,
        Ok(blocks) => {
            tracing::warn!(
                returned = blocks.len(),
                expected = commands.len(),
                "eth_simulateV1 returned mismatched block count, falling back to {L1_GAS_LIMIT_FALLBACK} per tx",
            );
            return Ok(vec![fallback_gas_limit; commands.len()]);
        }
        Err(err) => {
            tracing::warn!(
                %err,
                "eth_simulateV1 unavailable or errored, falling back to {L1_GAS_LIMIT_FALLBACK} per tx",
            );
            return Ok(vec![fallback_gas_limit; commands.len()]);
        }
    };

    let gas_limits = blocks
        .iter()
        .enumerate()
        .map(|(i, block)| match block.calls.first() {
            Some(call) if call.status => {
                if call.gas_used > block_gas_limit {
                    anyhow::bail!(
                        "simulated L1 transaction gas ({}) exceeds latest L1 block gas limit ({})",
                        call.gas_used,
                        block_gas_limit,
                    );
                }
                let padded_gas_limit = call.gas_used.saturating_mul(2);
                let gas_limit = padded_gas_limit.min(block_gas_limit);
                if gas_limit < padded_gas_limit {
                    tracing::warn!(
                        tx_index = i,
                        simulated_gas_used = call.gas_used,
                        padded_gas_limit,
                        block_gas_limit,
                        gas_limit,
                        "capping simulated L1 transaction gas limit at latest block gas limit"
                    );
                }
                Ok(gas_limit)
            }
            Some(call) => {
                tracing::warn!(
                    tx_index = i,
                    return_data = ?call.return_data,
                    "eth_simulateV1 call reverted; refusing to submit L1 transaction",
                );
                anyhow::bail!(
                    "eth_simulateV1 call at index {i} reverted; refusing to submit L1 transaction"
                );
            }
            None => {
                tracing::warn!(
                    tx_index = i,
                    "eth_simulateV1 block had no call result, falling back to {L1_GAS_LIMIT_FALLBACK}",
                );
                Ok(fallback_gas_limit)
            }
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(gas_limits)
}

async fn report_custom_priority_fee_metrics(provider: &dyn Provider) -> anyhow::Result<()> {
    for (window, blocks_behind) in [
        (PriorityFeeEstimateWindow::Blocks3, 3),
        (PriorityFeeEstimateWindow::Blocks5, 5),
        (PriorityFeeEstimateWindow::Blocks10, 10),
    ] {
        for (percentile_label, percentile) in [
            (PriorityFeeEstimatePercentile::P20, 20.0),
            (PriorityFeeEstimatePercentile::P30, 30.0),
            (PriorityFeeEstimatePercentile::P50, 50.0),
        ] {
            let our_eip1559_est =
                estimate_eip1559_fees(provider, blocks_behind, percentile).await?;
            L1_SENDER_METRICS.report_custom_estimated_max_priority_fee_per_gas(
                window,
                percentile_label,
                our_eip1559_est.max_priority_fee_per_gas,
            )?;
        }
    }
    Ok(())
}

/// Estimates EIP-1559 fees using the provided percentile of priority fees over the specified
/// fee-history window.
///
/// `estimate_eip1559_fees_with` in alloy hardcodes the block count and percentile, so we call
/// `get_fee_history` directly and delegate the rest to alloy's default estimator.
async fn estimate_eip1559_fees(
    provider: &dyn Provider,
    blocks_behind: u64,
    percentile: f64,
) -> anyhow::Result<Eip1559Estimation> {
    let fee_history = provider
        .get_fee_history(blocks_behind, BlockNumberOrTag::Latest, &[percentile])
        .await
        .context("fetching fee history")?;
    let base_fee_per_gas: u128 = fee_history.latest_block_base_fee().unwrap_or_default();
    let rewards = fee_history.reward.unwrap_or_default();
    Ok(alloy::providers::utils::eip1559_default_estimator(
        base_fee_per_gas,
        &rewards,
    ))
}

async fn register_operator<Input: SendToL1>(
    provider: &mut EthDynProvider,
    signer_config: SignerConfig,
) -> anyhow::Result<Address> {
    let address = signer_config
        .register_with_wallet(provider.wallet_mut())
        .await?;

    let balance = provider.get_balance(address).await?;
    L1_SENDER_METRICS.balance[&Input::COMPONENT_ID.as_str()].set(format_ether(balance).parse()?);
    let address_string: &'static str = address.to_string().leak();
    L1_SENDER_METRICS.l1_operator_address[&(Input::COMPONENT_ID.as_str(), address_string)].set(1);

    if balance.is_zero() {
        anyhow::bail!("L1 sender's address {address} has zero balance");
    }

    tracing::info!(
        command_name = Input::COMPONENT_ID.as_str(),
        balance_eth = format_ether(balance),
        %address,
        "initialized L1 sender",
    );
    Ok(address)
}

pub(crate) async fn report_operator_metrics_loop<P: Provider>(
    provider: P,
    operator_address: Address,
    command_name: &'static str,
) -> anyhow::Result<()> {
    let mut timer = tokio::time::interval(OPERATOR_METRICS_POLL_INTERVAL);
    loop {
        timer.tick().await;
        match provider.get_balance(operator_address).await {
            Ok(balance) => match format_ether(balance).parse() {
                Ok(balance) => {
                    L1_SENDER_METRICS.balance[&command_name].set(balance);
                }
                Err(err) => tracing::warn!(
                    command_name,
                    %operator_address,
                    "Failed to parse L1 operator balance metric: {err}"
                ),
            },
            Err(err) => tracing::warn!(
                command_name,
                %operator_address,
                "Failed to fetch L1 operator balance metric: {err}"
            ),
        }

        match provider.get_transaction_count(operator_address).await {
            Ok(nonce) => {
                L1_SENDER_METRICS.nonce[&command_name].set(nonce);
            }
            Err(err) => tracing::warn!(
                command_name,
                %operator_address,
                "Failed to fetch L1 operator nonce metric: {err}"
            ),
        }
    }
}

async fn validate_tx_receipt<Input: SendToL1>(
    provider: &impl Provider,
    command: &Input,
    receipt: TransactionReceipt,
) -> anyhow::Result<()> {
    let execution_fee = receipt.gas_used as u128 * receipt.effective_gas_price;
    let blob_fee = receipt
        .blob_gas_used
        .zip(receipt.blob_gas_price)
        .map(|(gas_used, gas_price)| gas_used as u128 * gas_price)
        .unwrap_or_default();
    let balance_consumed = execution_fee
        .saturating_add(blob_fee)
        .min(u128::from(u64::MAX)) as u64;

    L1_SENDER_METRICS.balance_consumed_by_tx[&Input::COMPONENT_ID.as_str()].set(balance_consumed);

    if receipt.status() {
        // Transaction succeeded - log output and return OK(())
        L1_SENDER_METRICS.report_tx_receipt(command, receipt)?;
        Ok(())
    } else {
        tracing::error!(
            %command,
            tx_hash = ?receipt.transaction_hash,
            l1_block_number = receipt.block_number.unwrap(),
            "Transaction failed on L1",
        );
        if let Ok(trace) = provider
            .debug_trace_transaction(
                receipt.transaction_hash,
                GethDebugTracingOptions::call_tracer(CallConfig::default()),
            )
            .await
        {
            let call_frame = trace
                .try_into_call_frame()
                .expect("requested call tracer but received a different call frame type");
            // We print top-level call frame's output as it likely contains serialized custom
            // error pointing to the underlying problem (i.e. starts with the error's 4byte
            // signature).
            tracing::error!(
                ?call_frame.output,
                ?call_frame.error,
                ?call_frame.revert_reason,
                "Failed transaction's top-level call frame"
            );
        }
        anyhow::bail!(
            "{} L1 command transaction failed, see L1 transaction's trace for more details (tx_hash='{:?}')",
            command,
            receipt.transaction_hash
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FeeParams, L1SenderFeeConfig, apply_fee_caps, is_retryable_gateway_da_admission_message,
        notify_commit_submitted_batch,
    };
    use crate::config::SYSCOIN_L1_PRIORITY_FEE_FLOOR_WEI;
    use alloy::providers::utils::Eip1559Estimation;
    use tokio::sync::watch;

    #[test]
    fn commit_submitted_marker_advances_to_recovered_batch() {
        let (tx, rx) = watch::channel(10);

        notify_commit_submitted_batch(&Some(tx), 11);

        assert_eq!(*rx.borrow(), 11);
    }

    #[test]
    fn commit_submitted_marker_never_moves_backward() {
        let (tx, rx) = watch::channel(10);

        notify_commit_submitted_batch(&Some(tx), 9);

        assert_eq!(*rx.borrow(), 10);
    }

    #[test]
    fn commit_submitted_marker_is_optional_for_non_commit_senders() {
        notify_commit_submitted_batch(&None, 11);
    }

    #[test]
    fn gateway_da_retry_matcher_only_accepts_availability_lag() {
        assert!(is_retryable_gateway_da_admission_message(
            "compact edge da admission check failed: compact edge da ref 0 (abc) is not retrievable",
        ));
        assert!(is_retryable_gateway_da_admission_message(
            "compact edge da admission check failed: compact edge da ref abc is temporarily cached as not retrievable",
        ));

        assert!(!is_retryable_gateway_da_admission_message(
            "compact edge da admission check failed: failed to decode compact edge da commit calldata: bad selector",
        ));
        assert!(!is_retryable_gateway_da_admission_message(
            "compact edge da admission check failed: compact edge da commitment mismatch",
        ));
        assert!(!is_retryable_gateway_da_admission_message(
            "compact edge da admission check failed: unsupported child-chain da commitment scheme",
        ));
    }

    #[test]
    fn apply_fee_caps_keeps_max_fee_and_blob_fee_static() {
        let configured = FeeParams {
            max_fee_per_gas: 100_000_000_000,
            max_priority_fee_per_gas: 2_000_000_000,
            max_fee_per_blob_gas: 50_000_000_000,
        };

        for estimated in [
            Eip1559Estimation {
                max_fee_per_gas: 1,
                max_priority_fee_per_gas: 1,
            },
            Eip1559Estimation {
                max_fee_per_gas: configured.max_fee_per_gas,
                max_priority_fee_per_gas: configured.max_priority_fee_per_gas,
            },
            Eip1559Estimation {
                max_fee_per_gas: configured.max_fee_per_gas * 10,
                max_priority_fee_per_gas: configured.max_priority_fee_per_gas * 10,
            },
        ] {
            let capped = apply_fee_caps(configured, estimated);
            assert_eq!(capped.max_fee_per_gas, configured.max_fee_per_gas);
            assert_eq!(capped.max_fee_per_blob_gas, configured.max_fee_per_blob_gas);
            assert!(capped.max_priority_fee_per_gas <= configured.max_priority_fee_per_gas);
        }
    }

    #[test]
    fn apply_fee_caps_enforces_syscoin_priority_fee_floor() {
        let configured = FeeParams {
            max_fee_per_gas: 100_000_000_000,
            max_priority_fee_per_gas: 2_000_000_000,
            max_fee_per_blob_gas: 50_000_000_000,
        };
        let estimated = Eip1559Estimation {
            max_fee_per_gas: 15,
            max_priority_fee_per_gas: 1,
        };

        let capped = apply_fee_caps(configured, estimated);

        assert_eq!(
            capped.max_priority_fee_per_gas,
            SYSCOIN_L1_PRIORITY_FEE_FLOOR_WEI
        );
    }

    #[test]
    fn replacement_fee_params_bump_from_syscoin_priority_fee_floor() {
        let fee_config = L1SenderFeeConfig {
            max_fee_per_gas_wei: 100_000,
            max_priority_fee_per_gas_wei: SYSCOIN_L1_PRIORITY_FEE_FLOOR_WEI,
            max_fee_per_blob_gas_wei: 50_000,
            max_fee_per_gas_replacement_multiplier: 1.1,
            max_priority_fee_per_gas_replacement_multiplier: 1.1,
            max_fee_per_blob_gas_replacement_multiplier: 2.0,
        };

        let replacement = fee_config.replacement_fee_params();

        assert_eq!(
            replacement.max_priority_fee_per_gas,
            (SYSCOIN_L1_PRIORITY_FEE_FLOOR_WEI as f64 * 1.1).ceil() as u128
        );
        assert!(replacement.max_fee_per_gas >= replacement.max_priority_fee_per_gas);
    }

    #[test]
    fn fee_config_rejects_caps_below_syscoin_priority_fee_floor() {
        let low_max_fee = L1SenderFeeConfig {
            max_fee_per_gas_wei: SYSCOIN_L1_PRIORITY_FEE_FLOOR_WEI - 1,
            max_priority_fee_per_gas_wei: SYSCOIN_L1_PRIORITY_FEE_FLOOR_WEI,
            max_fee_per_blob_gas_wei: 50_000,
            max_fee_per_gas_replacement_multiplier: 1.1,
            max_priority_fee_per_gas_replacement_multiplier: 1.1,
            max_fee_per_blob_gas_replacement_multiplier: 2.0,
        };
        assert!(low_max_fee.validate_syscoin_fee_caps().is_err());

        let low_priority_fee = L1SenderFeeConfig {
            max_fee_per_gas_wei: 100_000,
            max_priority_fee_per_gas_wei: SYSCOIN_L1_PRIORITY_FEE_FLOOR_WEI - 1,
            max_fee_per_blob_gas_wei: 50_000,
            max_fee_per_gas_replacement_multiplier: 1.1,
            max_priority_fee_per_gas_replacement_multiplier: 1.1,
            max_fee_per_blob_gas_replacement_multiplier: 2.0,
        };
        assert!(low_priority_fee.validate_syscoin_fee_caps().is_err());
    }

    #[test]
    fn fee_config_rejects_invalid_eip1559_caps() {
        let fee_config = L1SenderFeeConfig {
            max_fee_per_gas_wei: SYSCOIN_L1_PRIORITY_FEE_FLOOR_WEI,
            max_priority_fee_per_gas_wei: SYSCOIN_L1_PRIORITY_FEE_FLOOR_WEI + 1,
            max_fee_per_blob_gas_wei: 50_000,
            max_fee_per_gas_replacement_multiplier: 1.1,
            max_priority_fee_per_gas_replacement_multiplier: 1.1,
            max_fee_per_blob_gas_replacement_multiplier: 2.0,
        };

        assert!(fee_config.validate_syscoin_fee_caps().is_err());
    }

    #[test]
    fn fee_config_rejects_invalid_replacement_eip1559_caps() {
        let fee_config = L1SenderFeeConfig {
            max_fee_per_gas_wei: SYSCOIN_L1_PRIORITY_FEE_FLOOR_WEI,
            max_priority_fee_per_gas_wei: SYSCOIN_L1_PRIORITY_FEE_FLOOR_WEI,
            max_fee_per_blob_gas_wei: 50_000,
            max_fee_per_gas_replacement_multiplier: 1.1,
            max_priority_fee_per_gas_replacement_multiplier: 1.5,
            max_fee_per_blob_gas_replacement_multiplier: 2.0,
        };

        assert!(fee_config.validate_syscoin_fee_caps().is_err());
    }
}
