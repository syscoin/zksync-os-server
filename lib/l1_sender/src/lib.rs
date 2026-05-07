pub mod commands;
pub mod config;
mod metrics;
pub mod pipeline_component;
pub mod upgrade_gatekeeper;

use crate::commands::{L1SenderCommand, SendToL1};
use crate::config::L1SenderConfig;
use crate::metrics::{
    L1_SENDER_METRICS, L1SenderState, PriorityFeeEstimatePercentile, PriorityFeeEstimateWindow,
};
use alloy::consensus::BlobTransactionValidationError;
use alloy::consensus::Transaction as ConsensusTransaction;
use alloy::eips::eip7594::BlobTransactionSidecarVariant;
use alloy::eips::{BlockId, BlockNumberOrTag, Encodable2718};
use alloy::network::{
    BlockResponse, Ethereum, EthereumWallet, TransactionBuilder, TransactionBuilder4844,
    TransactionResponse,
};
use alloy::primitives::utils::{format_ether, format_units};
use alloy::primitives::{Address, B256};
use alloy::providers::ext::DebugApi;
use alloy::providers::fillers::{FillProvider, TxFiller};
use alloy::providers::utils::Eip1559Estimation;
use alloy::providers::{Provider, WalletProvider};
use alloy::rpc::types::trace::geth::{CallConfig, GethDebugTracingOptions};
use alloy::rpc::types::{TransactionReceipt, TransactionRequest};
use alloy::transports::TransportError;
use anyhow::Context as _;
use futures::future::BoxFuture;
use futures::{FutureExt, StreamExt, TryStreamExt};
use std::time::Instant;
use tokio::sync::mpsc::Sender;
use tokio::sync::watch;
use zksync_os_batch_types::batcher_model::{FriProof, SignedBatchEnvelope};
use zksync_os_observability::{ComponentStateHandle, ComponentStateReporter};
use zksync_os_operator_signer::SignerConfig;
use zksync_os_pipeline::PeekableReceiver;

/// A code for "method not found" error response as declared in JSON-RPC 2.0 spec.
const METHOD_NOT_FOUND_CODE: i64 = -32601;
/// SYSCOIN: future that resolves into a (fallible) transaction receipt wait outcome.
/// The outcome distinguishes confirmed txs from dropped txs so delayed inclusion is non-fatal.
type TransactionReceiptFuture = BoxFuture<'static, anyhow::Result<ReceiptWaitOutcome>>;
// SYSCOIN: track the optional raw signed tx and current hash so dropped txs can be rebroadcast
// or resubmitted without crashing the L1 sender.
type PendingTx<Input> = (
    TransactionReceiptFuture,
    Input,
    Instant,
    Option<Vec<u8>>,
    B256,
    u64,
);

// SYSCOIN: non-fatal receipt wait result used to recover from L1 mempool eviction.
enum ReceiptWaitOutcome {
    Confirmed(TransactionReceipt),
    Dropped,
}

const REQUIRED_CONFIRMATIONS_L1: u64 = 3;
/// In case there's only one chain connected to gateway, it is very likely that there will be not enough block production
/// to reach 3 confirmations for such transactions
const REQUIRED_CONFIRMATIONS_GATEWAY: u64 = 1;
/// SYSCOIN Extra headroom over the L1 RPC gas estimate.
const L1_TX_GAS_ESTIMATE_PADDING_NUMERATOR: u64 = 120;
const L1_TX_GAS_ESTIMATE_PADDING_DENOMINATOR: u64 = 100;
// SYSCOIN: fallback scan window when the provider does not support
// `eth_getTransactionBySenderAndNonce`.
const SAME_NONCE_MINED_TX_SCAN_DEPTH: u64 = 128;

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
pub async fn run_l1_sender<Input: SendToL1>(
    // == plumbing ==
    mut inbound: PeekableReceiver<L1SenderCommand<Input>>,
    outbound: Sender<SignedBatchEnvelope<FriProof>>,

    // == command-specific settings ==
    to_address: Address,

    // == config ==
    mut provider: FillProvider<
        impl TxFiller<Ethereum> + WalletProvider<Wallet = EthereumWallet>,
        impl Provider<Ethereum>,
    >,
    config: L1SenderConfig<Input>,
    gateway: bool,
    commit_submitted_tx: Option<watch::Sender<u64>>,
    // The SL block number at which `getTotalBatches*` was called on startup. Pinning the
    // confirmed-nonce baseline to this block ensures it is consistent with where the
    // inbound command queue begins — avoiding a crash caused by txs that are mined between
    // the `getTotalBatches` call and the nonce check.
    sl_block_number: u64,
) -> anyhow::Result<()> {
    let latency_tracker =
        ComponentStateReporter::global().handle_for(Input::NAME, L1SenderState::WaitingRecv);
    let command_name = Input::NAME;

    // SYSCOIN: keep `config` available after operator registration because dropped-tx recovery
    // can resubmit commands through the same config.
    let operator_address =
        register_operator::<_, Input>(&mut provider, config.operator_signer.clone()).await?;
    let mut cmd_buffer = Vec::with_capacity(config.command_limit);
    // Process all potential passthrough commands first
    if process_prepending_passthrough_commands(
        &mut inbound,
        &outbound,
        &latency_tracker,
        command_name,
    )
    .await?
    .is_none()
    {
        tracing::info!("inbound channel closed");
        return Ok(());
    }

    // On startup, detect any L1 transactions that were submitted in a previous session
    // but not yet mined, and pair them with the corresponding queued commands.
    let recovered = match recover_in_flight_txs(
        &provider,
        operator_address,
        gateway,
        &mut inbound,
        command_name,
        sl_block_number,
    )
    .await
    {
        Ok(paired) => paired,
        Err(err) => {
            tracing::warn!("Error during in-flight transaction recovery: {err}");
            vec![]
        }
    };

    // Wait for any recovered in-flight transactions to be mined before accepting
    // new commands. Their nonces precede anything we are about to send, so they
    // must be confirmed first.
    if !recovered.is_empty() {
        let pending_txs: Vec<PendingTx<Input>> = recovered
            .into_iter()
            .map(|(tx_hash, cmd, nonce)| {
                let fut = wait_for_confirmed_receipt(
                    provider.root().clone(),
                    tx_hash,
                    if gateway {
                        REQUIRED_CONFIRMATIONS_GATEWAY
                    } else {
                        REQUIRED_CONFIRMATIONS_L1
                    },
                    config.transaction_timeout,
                )
                .boxed();
                // SYSCOIN: recovered in-flight txs have no raw signed payload; if they disappear,
                // recovery resubmits from the queued command instead.
                (fut, cmd, Instant::now(), None, tx_hash, nonce)
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
            if gateway {
                REQUIRED_CONFIRMATIONS_GATEWAY
            } else {
                REQUIRED_CONFIRMATIONS_L1
            },
            config.transaction_timeout,
            &latency_tracker,
            &outbound,
        )
        .await?;
    }

    // At this point, all in-flight transactions from the previous session are confirmed.
    // Only actual SendToL1 commands are expected from here on.
    loop {
        latency_tracker.enter_state(L1SenderState::WaitingRecv);
        // This sleeps until **at least one** command is received from the channel. Additionally,
        // receives up to `self.command_limit` commands from the channel if they are ready (i.e. does
        // not wait for them). Extends `cmd_buffer` with received values and, as `cmd_buffer` is
        // emptied in every iteration, its size never exceeds `self.command_limit`.
        // SYSCOIN: execute appends to MessageRoot sequentially, so tx N+1
        // cannot be prepared before tx N is mined. Keep commit/prove pipelining intact.
        let command_limit = if command_name == "execute" {
            1
        } else {
            config.command_limit
        };
        let received = inbound.recv_many(&mut cmd_buffer, command_limit).await;
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
        // This method only returns `0` if the channel has been closed and there are no more items
        // in the queue.
        if received == 0 {
            tracing::info!("inbound channel closed");
            return Ok(());
        }
        latency_tracker.enter_state(L1SenderState::SendingToL1);
        let range = Input::display_range(&commands); // Only for logging
        tracing::info!(command_name, range, "sending L1 transactions");
        L1_SENDER_METRICS.parallel_transactions[&command_name].set(commands.len() as u64);
        // It's important to preserve the order of commands -
        // so that we send them downstream also in order.
        // This holds true because l1 transactions are included in the order of sender nonce.
        // Keep this in mind if changing sending logic (that is, if adding `buffer` we'd need to set nonce manually)
        // SYSCOIN: submit via a helper so dropped-tx recovery can reuse the exact same path.
        let pending_txs: Vec<PendingTx<Input>> = futures::stream::iter(commands.drain(..))
            .then(|mut cmd| async {
                let (receipt_fut, submitted_at, raw_tx, tx_hash, tx_nonce) = submit_l1_transaction(
                    &provider,
                    operator_address,
                    to_address,
                    &config,
                    gateway,
                    command_name,
                    &mut cmd,
                    &commit_submitted_tx,
                )
                .await?;
                anyhow::Ok((receipt_fut, cmd, submitted_at, raw_tx, tx_hash, tx_nonce))
            })
            // We could buffer the stream here to enable sending multiple batches of transactions in parallel,
            // but this is not necessary for now - we wait for them to be included in parallel
            .try_collect::<Vec<_>>()
            .await?;
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
            if gateway {
                REQUIRED_CONFIRMATIONS_GATEWAY
            } else {
                REQUIRED_CONFIRMATIONS_L1
            },
            config.transaction_timeout,
            &latency_tracker,
            &outbound,
        )
        .await?;
    }
}

// SYSCOIN: common L1 tx submission path used by the normal loop and by dropped-tx recovery.
async fn submit_l1_transaction<F, P, Input>(
    provider: &FillProvider<F, P>,
    operator_address: Address,
    to_address: Address,
    config: &L1SenderConfig<Input>,
    gateway: bool,
    command_name: &'static str,
    cmd: &mut Input,
    commit_submitted_tx: &Option<watch::Sender<u64>>,
) -> anyhow::Result<(
    TransactionReceiptFuture,
    Instant,
    Option<Vec<u8>>,
    B256,
    u64,
)>
where
    F: TxFiller<Ethereum> + WalletProvider<Wallet = EthereumWallet>,
    P: Provider<Ethereum>,
    Input: SendToL1,
{
    let tx_range = Input::display_range(std::slice::from_ref(cmd));
    let mut tx_request = tx_request_with_gas_fields(
        provider,
        operator_address,
        config.max_fee_per_gas_wei,
        config.max_priority_fee_per_gas_wei,
    )
    .await?
    .with_to(to_address)
    .with_input(cmd.solidity_call(gateway, &operator_address));

    if let Some(blob_sidecar) = cmd.blob_sidecar() {
        let fee_per_blob_gas = provider.get_blob_base_fee().await?;
        L1_SENDER_METRICS.report_blob_base_fee(fee_per_blob_gas)?;
        let max_fee_per_blob_gas = config.max_fee_per_blob_gas_wei;

        if fee_per_blob_gas > max_fee_per_blob_gas {
            tracing::warn!(
                max_fee_per_blob_gas,
                fee_per_blob_gas,
                "L1 sender's configured maxFeePerBlobGas is lower than the one estimated from network"
            );
        }
        tx_request.set_max_fee_per_blob_gas(max_fee_per_blob_gas);
        tx_request.set_blob_sidecar(blob_sidecar);
    };

    apply_l1_gas_limit(provider, &mut tx_request).await?;

    // Fill the transaction (e.g., nonce, gas, etc.) using the provider and convert it to an
    // envelope.
    let envelope = provider
        .fill(tx_request)
        .await?
        .try_into_envelope()?
        .try_into_pooled()?;

    let pending_block = provider
        .get_block(BlockId::pending())
        .await?
        .expect("no pending block");
    // todo: make conversion unconditional (and remove respective config) once anvil
    //       supports EIP-7594 blobs (see https://github.com/foundry-rs/foundry/issues/12222)
    let tx = if config.fusaka_upgrade_timestamp <= pending_block.header.timestamp {
        // Convert the envelope into an EIP-7594 transaction by converting the sidecar
        envelope.try_map_eip4844(|tx| {
            tx.try_map_sidecar(|sidecar| {
                Ok::<_, BlobTransactionValidationError>(BlobTransactionSidecarVariant::Eip7594(
                    sidecar.try_into_eip7594()?,
                ))
            })
        })?
    } else {
        // Keep the regular EIP-4844 sidecar
        envelope
    };

    let raw_tx = tx.encoded_2718();
    let tx_nonce = tx.nonce();
    let pending_tx = provider.send_raw_transaction(&raw_tx).await?;
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
    )
    .boxed();
    tracing::info!(
        "{command_name}: L1 transaction submitted for {tx_range}. Hash: {tx_hash:?} Waiting for inclusion...",
    );

    // Notify CommitWatcher: this batch number has been submitted to L1.
    if let Some(sender) = commit_submitted_tx {
        let batch_number = cmd
            .as_ref()
            .last()
            .expect("commands is non-empty after recv_many")
            .batch_number();
        sender.send_if_modified(|current| {
            if batch_number > *current {
                *current = batch_number;
                true
            } else {
                false
            }
        });
    }

    cmd.as_mut()
        .iter_mut()
        .for_each(|envelope| envelope.set_stage(Input::SENT_STAGE));

    // SYSCOIN: retain raw signed tx bytes for safe same-hash rebroadcast when the provider
    // reports the transaction as dropped before a receipt appears.
    Ok((receipt_fut, submitted_at, Some(raw_tx), tx_hash, tx_nonce))
}

/// Waits for all pending L1 transaction receipts, validates them, logs balance/nonce
/// metrics, and forwards the completed commands downstream.
async fn wait_for_txs_and_forward<F, P, Input>(
    pending_txs: Vec<PendingTx<Input>>,
    provider: &FillProvider<F, P>,
    operator_address: Address,
    to_address: Address,
    config: &L1SenderConfig<Input>,
    gateway: bool,
    commit_submitted_tx: &Option<watch::Sender<u64>>,
    command_name: &'static str,
    required_confirmations: u64,
    transaction_timeout: std::time::Duration,
    latency_tracker: &ComponentStateHandle<L1SenderState>,
    outbound: &Sender<SignedBatchEnvelope<FriProof>>,
) -> anyhow::Result<()>
where
    F: TxFiller<Ethereum> + WalletProvider<Wallet = EthereumWallet>,
    P: Provider<Ethereum>,
    Input: SendToL1,
{
    latency_tracker.enter_state(L1SenderState::WaitingL1Inclusion);

    let mut completed_commands = Vec::with_capacity(pending_txs.len());
    for (mut receipt_fut, mut command, mut submitted_at, mut raw_tx, mut tx_hash, mut tx_nonce) in
        pending_txs
    {
        let receipt = loop {
            let receipt = receipt_fut.await;
            // Observe latency before propagating errors so provider/RPC failures are recorded.
            L1_SENDER_METRICS.tx_inclusion_latency_seconds[&command_name]
                .observe(submitted_at.elapsed().as_secs_f64());
            match receipt? {
                ReceiptWaitOutcome::Confirmed(receipt) => break receipt,
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
                        )
                        .await?;
                        receipt_fut = resubmitted.0;
                        submitted_at = resubmitted.1;
                        raw_tx = resubmitted.2;
                        tx_hash = resubmitted.3;
                        tx_nonce = resubmitted.4;
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
                                    tx_nonce,
                                    tx_hash,
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
                                )
                                .await?;
                                receipt_fut = resubmitted.0;
                                submitted_at = resubmitted.1;
                                raw_tx = resubmitted.2;
                                tx_hash = resubmitted.3;
                                tx_nonce = resubmitted.4;
                                continue;
                            }
                        }
                    }
                    receipt_fut = wait_for_confirmed_receipt(
                        provider.root().clone(),
                        tx_hash,
                        required_confirmations,
                        transaction_timeout,
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

    latency_tracker.enter_state(L1SenderState::WaitingSend);
    for command in completed_commands {
        for mut output_envelope in command.into() {
            output_envelope.set_stage(Input::MINED_STAGE);
            outbound.send(output_envelope).await?;
        }
    }
    Ok(())
}

// SYSCOIN: nonce-reuse rebroadcast errors mean the original nonce may already be occupied.
// Keep looking for the same-nonce tx instead of resubmitting the command at a later nonce or
// re-arming a waiter for the dropped hash.
async fn recover_same_nonce_tx<F, P, Input>(
    provider: &FillProvider<F, P>,
    operator_address: Address,
    nonce: u64,
    old_tx_hash: B256,
    gateway: bool,
    command_name: &'static str,
    command: &Input,
    timeout: std::time::Duration,
    rebroadcast_err: &TransportError,
) -> anyhow::Result<B256>
where
    F: TxFiller<Ethereum> + WalletProvider<Wallet = EthereumWallet>,
    P: Provider<Ethereum>,
    Input: SendToL1,
{
    let started_at = Instant::now();
    let poll_interval = provider.client().poll_interval();
    let mut next_warning_at = if timeout.is_zero() {
        None
    } else {
        Some(timeout)
    };

    loop {
        match find_matching_sender_nonce_tx(
            provider,
            operator_address,
            nonce,
            gateway,
            command_name,
            command,
        )
        .await?
        {
            SameNonceTx::Found(tx_hash) => return Ok(tx_hash),
            SameNonceTx::NotFound => {
                let elapsed = started_at.elapsed();
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
                if let Some(warning_at) = next_warning_at
                    && elapsed >= warning_at
                {
                    tracing::warn!(
                        command_name,
                        ?old_tx_hash,
                        nonce,
                        waited_secs = elapsed.as_secs_f64(),
                        "L1 transaction rebroadcast returned a nonce-reuse error and \
                         eth_getTransactionBySenderAndNonce is unsupported; retrying standard \
                         block-scan recovery: {rebroadcast_err}",
                    );
                    next_warning_at = Some(warning_at + timeout);
                }
                tokio::time::sleep(poll_interval).await;
            }
        }
    }
}

// SYSCOIN: standard-RPC fallback for providers that do not implement sender+nonce lookup.
// Scan recent mined blocks and accept only a transaction with the same sender, nonce, and calldata.
async fn find_matching_mined_sender_nonce_tx<F, P, Input>(
    provider: &FillProvider<F, P>,
    operator_address: Address,
    nonce: u64,
    gateway: bool,
    command_name: &'static str,
    command: &Input,
) -> anyhow::Result<Option<B256>>
where
    F: TxFiller<Ethereum> + WalletProvider<Wallet = EthereumWallet>,
    P: Provider<Ethereum>,
    Input: SendToL1,
{
    let latest_block = provider.get_block_number().await?;
    let first_block = latest_block.saturating_sub(SAME_NONCE_MINED_TX_SCAN_DEPTH);
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

// SYSCOIN: only errors that indicate the exact raw tx is still known are benign. Nonce-conflict
// or underpriced replacement errors must use the resubmission path instead of waiting on a stale hash.
fn is_benign_rebroadcast_error(err: &TransportError) -> bool {
    match err {
        TransportError::ErrorResp(resp) => {
            let message = resp.message.to_ascii_lowercase();
            message.contains("already known")
                || message.contains("known transaction")
                || message.contains("already imported")
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

// SYSCOIN: outcome of same-nonce discovery after a nonce-reuse rebroadcast error.
enum SameNonceTx {
    Found(B256),
    NotFound,
    Unsupported,
}

// SYSCOIN: if a rebroadcast reports nonce reuse, try to discover the tx currently occupying the
// original sender nonce and track it only if it carries the same command calldata.
async fn find_matching_sender_nonce_tx<F, P, Input>(
    provider: &FillProvider<F, P>,
    operator_address: Address,
    nonce: u64,
    gateway: bool,
    command_name: &'static str,
    command: &Input,
) -> anyhow::Result<SameNonceTx>
where
    F: TxFiller<Ethereum> + WalletProvider<Wallet = EthereumWallet>,
    P: Provider<Ethereum>,
    Input: SendToL1,
{
    let tx = match provider
        .get_transaction_by_sender_nonce(operator_address, nonce)
        .await
    {
        Ok(tx) => tx,
        Err(TransportError::ErrorResp(ref e)) if e.code == METHOD_NOT_FOUND_CODE => {
            tracing::warn!(
                command_name,
                nonce,
                scan_depth = SAME_NONCE_MINED_TX_SCAN_DEPTH,
                "eth_getTransactionBySenderAndNonce is not supported; scanning recent mined blocks",
            );
            return find_matching_mined_sender_nonce_tx(
                provider,
                operator_address,
                nonce,
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
) -> anyhow::Result<ReceiptWaitOutcome>
where
    P: Provider<Ethereum>,
{
    let started_at = Instant::now();
    let poll_interval = provider.client().poll_interval();
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
        }

        let elapsed = started_at.elapsed();
        let receipt_block_number = receipt.as_ref().and_then(|receipt| receipt.block_number);
        let confirmed_at =
            receipt_block_number.map(|block| block + required_confirmations.saturating_sub(1));
        // SYSCOIN: delayed L1 inclusion is an operational condition, not a fatal sender error.
        // Keep waiting for the nonce-bearing transaction so congestion/censorship cannot crash
        // the main node; use `transaction_timeout` only as the repeated warning interval.
        if let Some(warning_at) = next_warning_at
            && elapsed >= warning_at
        {
            if receipt.is_none() {
                match provider.get_transaction_by_hash(tx_hash).await {
                    Ok(None) => {
                        tracing::warn!(
                            "L1 transaction {tx_hash} is no longer visible by hash after \
                             waiting for confirmation; it will be rebroadcast if possible"
                        );
                        return Ok(ReceiptWaitOutcome::Dropped);
                    }
                    Ok(Some(_)) => {}
                    Err(err) => {
                        tracing::warn!(
                            "Failed to check whether L1 transaction {tx_hash} is still visible \
                             while waiting for confirmation: {err}"
                        );
                    }
                }
            }
            tracing::warn!(
                "Still waiting for L1 transaction confirmation for tx {tx_hash}. \
                 required_confirmations={required_confirmations}, \
                 waited_secs={}, latest_l1_block={latest_block}, \
                 receipt_block_number={receipt_block_number:?}, confirmed_at={confirmed_at:?}",
                elapsed.as_secs_f64(),
            );
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
async fn recover_in_flight_txs<F, P, Input>(
    provider: &FillProvider<F, P>,
    operator_address: Address,
    gateway: bool,
    inbound: &mut PeekableReceiver<L1SenderCommand<Input>>,
    command_name: &str,
    sl_block_number: u64,
) -> anyhow::Result<Vec<(alloy::primitives::B256, Input, u64)>>
where
    F: TxFiller<Ethereum> + WalletProvider<Wallet = EthereumWallet>,
    P: Provider<Ethereum>,
    Input: SendToL1,
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
                let Some(L1SenderCommand::SendToL1(cmd)) = inbound.recv().await else {
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

async fn process_prepending_passthrough_commands<Input: SendToL1>(
    inbound: &mut PeekableReceiver<L1SenderCommand<Input>>,
    outbound: &Sender<SignedBatchEnvelope<FriProof>>,
    latency_tracker: &ComponentStateHandle<L1SenderState>,
    command_name: &str,
) -> anyhow::Result<Option<()>> {
    loop {
        latency_tracker.enter_state(L1SenderState::WaitingRecv);
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
                let Some(next_command) = inbound.recv().await else {
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
                        latency_tracker.enter_state(L1SenderState::WaitingSend);
                        outbound
                            .send((*batch).with_stage(Input::PASSTHROUGH_STAGE))
                            .await?;
                    }
                }
            }
        }
    }
}

async fn tx_request_with_gas_fields(
    provider: &dyn Provider,
    operator_address: Address,
    max_fee_per_gas: u128,
    max_priority_fee_per_gas: u128,
) -> anyhow::Result<TransactionRequest> {
    let eip1559_est = provider.estimate_eip1559_fees().await?;
    L1_SENDER_METRICS.report_l1_eip_1559_estimation(eip1559_est)?;
    report_custom_priority_fee_metrics(provider).await?;

    tracing::debug!(
        max_priority_fee_per_gas_gwei = ?format_units(eip1559_est.max_priority_fee_per_gas, "gwei"),
        max_fee_per_gas_gwei = ?format_units(eip1559_est.max_fee_per_gas, "gwei"),
        "estimated priority and max fees"
    );
    // SYSCOIN Use configured values as floors to avoid underpriced transactions on networks
    // where fee estimators return unrealistically low values.
    let capped_max_fee_per_gas = if eip1559_est.max_fee_per_gas < max_fee_per_gas {
        tracing::warn!(
            "L1 sender's configured maxFeePerGas ({max_fee_per_gas}) \
             is lower than the one estimated from network  ({}), \
             using the configured base fee value ({max_fee_per_gas}) - this may result in inclusion delay.",
            eip1559_est.max_fee_per_gas
        );
        max_fee_per_gas
    } else {
        eip1559_est.max_fee_per_gas
    };
    let capped_max_priority_fee_per_gas = if eip1559_est.max_priority_fee_per_gas
        < max_priority_fee_per_gas
    {
        tracing::warn!(
            "L1 sender's configured max_priority_fee_per_gas ({max_priority_fee_per_gas}) \
             is lower than the one estimated from network  ({}), \
             using the configured priority fee value ({max_priority_fee_per_gas}) - this may result in inclusion delay.",
            eip1559_est.max_priority_fee_per_gas
        );
        max_priority_fee_per_gas
    } else {
        eip1559_est.max_priority_fee_per_gas
    };

    let tx = TransactionRequest::default()
        .with_from(operator_address)
        .with_max_fee_per_gas(capped_max_fee_per_gas)
        .with_max_priority_fee_per_gas(capped_max_priority_fee_per_gas);
    Ok(tx)
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

async fn register_operator<
    P: Provider + WalletProvider<Wallet = EthereumWallet>,
    Input: SendToL1,
>(
    provider: &mut P,
    signer_config: SignerConfig,
) -> anyhow::Result<Address> {
    let address = signer_config
        .register_with_wallet(provider.wallet_mut())
        .await?;

    let balance = provider.get_balance(address).await?;
    L1_SENDER_METRICS.balance[&Input::NAME].set(format_ether(balance).parse()?);
    let address_string: &'static str = address.to_string().leak();
    L1_SENDER_METRICS.l1_operator_address[&(Input::NAME, address_string)].set(1);

    if balance.is_zero() {
        anyhow::bail!("L1 sender's address {address} has zero balance");
    }

    tracing::info!(
        command_name = Input::NAME,
        balance_eth = format_ether(balance),
        %address,
        "initialized L1 sender",
    );
    Ok(address)
}

async fn validate_tx_receipt<Input: SendToL1>(
    provider: &impl Provider,
    command: &Input,
    receipt: TransactionReceipt,
) -> anyhow::Result<()> {
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
