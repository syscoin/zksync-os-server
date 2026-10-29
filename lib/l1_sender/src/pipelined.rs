//! Pipelined ("two-watermark") L1 sending.
//!
//! Submission, inclusion tracking, and confirmation run as concurrent tasks joined by
//! bounded queues, so new transactions go out while earlier ones are still waiting to be
//! mined or confirmed:
//!
//! ```text
//! submitter ──(submitted queue: entries hold an unmined-slot permit)──▶ inclusion watcher
//! inclusion watcher ──(mined queue)──▶ confirmation forwarder ──▶ downstream
//! confirmation forwarder ──(watch: confirmed-nonce floor)──▶ submitter (sim-prefix pruning)
//! ```
//!
//! The two watermarks:
//!
//! * **Unmined window** — at most `command_limit` transactions may be submitted but not yet
//!   observed mined. This matches the L1 pool's per-account cap, which counts pooled (unmined)
//!   transactions only. Each submission takes a semaphore permit that the inclusion watcher
//!   releases at the *first receipt sighting* — never at confirmation, which would keep each
//!   window slot occupied for `required_confirmations - 1` extra blocks per transaction.
//! * **Confirmation depth** — downstream forwarding still waits for `required_confirmations`,
//!   handled by a separate FIFO task so it delays forwarding by a constant without ever
//!   blocking submission.
//!
//! Invariants:
//!
//! * Nonces are assigned strictly sequentially by the single submitter; `next_nonce` advances
//!   only on a successful send. Any ambiguous send outcome (transport error) is fatal —
//!   restart + in-flight recovery is the only safe way to resolve it.
//! * Downstream forwarding is strict FIFO = nonce order = batch order (same-sender L1
//!   transactions are included in nonce order, so receipts appear in FIFO order too).
//! * Transient window overshoot (an L1 reorg can briefly return "mined" transactions to the
//!   pool after their permits were released) is absorbed by treating pool-capacity rejections
//!   as retryable with a generous budget.
//! * A transaction evicted from the L1 pool (fee spike, pool pressure) is detected by the
//!   inclusion watcher and resent at its nonce — waiting for its receipt would wedge the
//!   window forever. The old hash stays tracked in case the evicted original still mines.
//! * The L1 RPC endpoint is assumed to serve a single consistent view (one node, or sticky
//!   routing to one): startup recovery trusts a single `pending` nonce read. A backend that
//!   has not seen our pooled transactions makes the sender race its own earlier sends —
//!   still safe (single-use nonces + the contract's batch-chain check revert anything
//!   inconsistent), but each affected nonce costs wasted gas and a spurious restart.

use crate::commands::{L1SenderCommand, SendToL1};
use crate::metrics::L1_SENDER_METRICS;
use crate::pipeline_component::L1Sender;
use crate::{
    ConfirmationResend, FeeParams, L1SenderState, METHOD_NOT_FOUND_CODE, PendingSimulationPrefix,
    SimPrefixEntry,
};
use alloy::consensus::Transaction as ConsensusTransaction;
use alloy::network::{Ethereum, Network, TransactionResponse};
use alloy::primitives::{Address, B256};
use alloy::providers::Provider;
use alloy::transports::TransportError;
use anyhow::Context as _;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};
use zksync_os_batch_types::batcher_model::{FriProof, SignedBatchEnvelope};
use zksync_os_observability::ComponentStateReporter;
use zksync_os_pipeline::{PeekableReceiver, SendAndRecordExt};

pub(crate) type L1TxResponse = <Ethereum as Network>::TransactionResponse;

/// Sizing factor for the mined-but-unconfirmed queue. The physical population is bounded by
/// (inclusion rate × confirmation window); the bound only exists to cap memory if the
/// confirmation forwarder stalls, at which point permit starvation pauses submission.
const MINED_QUEUE_CAPACITY_FACTOR: usize = 4;

/// A transaction submitted to L1 but not yet observed mined. Holds one unmined-window permit,
/// released by the inclusion watcher at the first receipt sighting.
struct InFlightTx<Input> {
    /// Candidate hashes for this nonce, oldest first. Grows when a pool-evicted transaction
    /// is resent: the old hash stays a candidate because eviction is observed remotely and
    /// the original may still mine (e.g. it was propagated before the eviction).
    tx_hashes: Vec<B256>,
    nonce: u64,
    command: Input,
    submitted_at: Instant,
    request: alloy::rpc::types::TransactionRequest,
    fee_params: FeeParams,
    raw_transactions: Vec<(B256, alloy::primitives::Bytes)>,
    permit: OwnedSemaphorePermit,
}

/// Inclusion-watcher → submitter request to resend the transaction at `nonce` (its previous
/// send was evicted from the L1 pool). The submitter rebuilds it from the nonce's
/// simulation-prefix entry and replies with the new hash.
struct ResendRequest {
    nonce: u64,
    reply: tokio::sync::oneshot::Sender<(B256, FeeParams, Option<alloy::primitives::Bytes>)>,
}

enum PrefixProgress {
    Confirmed,
    Resend(ResendRequest),
}

struct PrefixProgressReceivers<'a> {
    confirmed_floor: &'a mut watch::Receiver<u64>,
    resend: &'a mut mpsc::Receiver<ResendRequest>,
}

async fn wait_for_prefix_progress(
    confirmed_floor: &mut watch::Receiver<u64>,
    resend_rx: &mut mpsc::Receiver<ResendRequest>,
) -> anyhow::Result<PrefixProgress> {
    tokio::select! {
        biased;
        Some(request) = resend_rx.recv() => Ok(PrefixProgress::Resend(request)),
        changed = confirmed_floor.changed() => {
            changed.context("confirmation forwarder terminated")?;
            Ok(PrefixProgress::Confirmed)
        }
    }
}

/// A transaction observed mined, awaiting `required_confirmations` before its command is
/// forwarded downstream.
struct MinedTx<Input> {
    tx_hashes: Vec<B256>,
    nonce: u64,
    command: Input,
    submitted_at: Instant,
    resend: ConfirmationResend,
}

/// Fee floor applying while the submitter replaces a stale in-flight suffix left by a previous
/// session (recovery calldata mismatch or a dropped transaction). The floor is the stale
/// transactions' actual fees bumped by the regular transaction-pool price-bump rule.
#[derive(Clone, Copy, Debug)]
pub(super) struct ReplacementPlan {
    /// Nonces below this replace previously-submitted transactions.
    until_nonce: u64,
    fee_floor: FeeParams,
}

/// A previous session's in-flight transaction paired with its queued command during recovery.
struct RecoveredInFlight<Input> {
    tx_hash: B256,
    nonce: u64,
    command: Input,
    /// Fees the transaction was actually sent with — reused for its simulation-prefix entry.
    fee_params: FeeParams,
    /// Gas limit the transaction was sent with — reused if it must be resent after eviction.
    gas_limit: u64,
}

/// Startup state for the pipelined sender, derived from in-flight transaction recovery.
struct PipelinedStart<Input> {
    /// Already-submitted transactions to track (nonce order). Their commands were consumed
    /// from the inbound queue.
    seeds: Vec<RecoveredInFlight<Input>>,
    /// Nonce of the next new submission.
    next_nonce: u64,
    /// All nonces below this are known mined (confirmed-nonce baseline at startup).
    mined_floor: u64,
    replacement: Option<ReplacementPlan>,
}

/// Mutable submitter-side state threaded through send waves.
struct SubmitterState {
    next_nonce: u64,
    /// Submitted-but-unconfirmed transactions mirrored into `eth_simulateV1` payloads
    /// (see [`SimPrefixEntry`]). Pruned once the confirmation forwarder advances.
    in_flight_prefix: VecDeque<SimPrefixEntry>,
    replacement: Option<ReplacementPlan>,
}

pub(crate) fn prune_confirmed_prefix(prefix: &mut VecDeque<SimPrefixEntry>, floor: u64) -> usize {
    while prefix.front().is_some_and(|entry| entry.nonce < floor) {
        prefix.pop_front();
    }
    256_usize.saturating_sub(prefix.len())
}

#[cfg(test)]
mod prefix_progress_tests {
    use super::*;

    #[tokio::test]
    async fn waiting_services_resends_and_wakes_on_confirmation() {
        let (floor_tx, mut floor_rx) = watch::channel(7);
        let (resend_tx, mut resend_rx) = mpsc::channel(1);
        let (reply, _reply_rx) = tokio::sync::oneshot::channel();
        {
            let wait = wait_for_prefix_progress(&mut floor_rx, &mut resend_rx);
            tokio::pin!(wait);
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(20), &mut wait)
                    .await
                    .is_err()
            );
            resend_tx
                .send(ResendRequest { nonce: 7, reply })
                .await
                .unwrap();
            match wait.await.unwrap() {
                PrefixProgress::Resend(request) => assert_eq!(request.nonce, 7),
                PrefixProgress::Confirmed => panic!("confirmation did not advance"),
            }
        }
        floor_tx.send_replace(8);
        assert!(matches!(
            wait_for_prefix_progress(&mut floor_rx, &mut resend_rx)
                .await
                .unwrap(),
            PrefixProgress::Confirmed
        ));
        drop(floor_tx);
        assert!(
            wait_for_prefix_progress(&mut floor_rx, &mut resend_rx)
                .await
                .is_err()
        );
    }
}

impl<Input> L1Sender<Input>
where
    Input: SendToL1 + Send + 'static,
{
    pub(crate) async fn run_pipelined(
        &self,
        mut inbound: PeekableReceiver<L1SenderCommand<Input>>,
        outbound: mpsc::Sender<SignedBatchEnvelope<FriProof>>,
        state_reporter: ComponentStateReporter,
        latest_nonce: u64,
    ) -> anyhow::Result<()> {
        let command_name = Input::COMPONENT_ID.as_str();
        let operator_address = self.operator_address().await?;
        // SYSCOIN: direct-L1 execution is safe to pipeline through upstream's simulation
        // prefix. Gateway execution still depends on the prior MessageRoot append being mined.
        let window = if self.gateway
            && Input::COMPONENT_ID == zksync_os_pipeline::ComponentId::L1SenderExecute
        {
            1
        } else {
            self.config.command_limit.max(1)
        };

        let start = self
            .plan_pipelined_recovery(
                &mut inbound,
                &state_reporter,
                operator_address,
                latest_nonce,
            )
            .await?
            .context("inbound channel closed during in-flight recovery")?;

        tracing::info!(
            command_name,
            window,
            seeds = start.seeds.len(),
            next_nonce = start.next_nonce,
            replacement = ?start.replacement,
            "starting pipelined L1 sender",
        );

        let slots = Arc::new(Semaphore::new(window));
        // Entries hold an unmined-slot permit, so occupancy never exceeds `window`.
        let (submitted_tx, mut submitted_rx) = mpsc::channel::<InFlightTx<Input>>(window);
        let (mined_tx, mut mined_rx) =
            mpsc::channel::<MinedTx<Input>>(MINED_QUEUE_CAPACITY_FACTOR * window + 64);
        // Prefixes remain necessary until the predecessor is canonically confirmed.
        let (mined_floor_tx, mut mined_floor_rx) = watch::channel(start.mined_floor);
        // Inclusion watcher asks the submitter to resend pool-evicted transactions. The
        // watcher requests resends one at a time (head of line), so capacity 1 suffices.
        let (resend_tx, mut resend_rx) = mpsc::channel::<ResendRequest>(1);

        let state_reporter = &state_reporter;

        let submitter = {
            let slots = Arc::clone(&slots);
            let submitted_tx = submitted_tx;
            async move {
                let mut state = SubmitterState {
                    next_nonce: start.next_nonce,
                    in_flight_prefix: VecDeque::new(),
                    replacement: start.replacement,
                };
                self.seed_recovered_txs(
                    start.seeds,
                    &mut state,
                    &slots,
                    &submitted_tx,
                    &mut resend_rx,
                    operator_address,
                )
                .await?;

                let mut raw_buffer: Vec<L1SenderCommand<Input>> = Vec::with_capacity(window);
                loop {
                    loop {
                        let floor = *mined_floor_rx.borrow_and_update();
                        if prune_confirmed_prefix(&mut state.in_flight_prefix, floor) > 0 {
                            break;
                        }
                        tokio::select! {
                            biased;
                            Some(request) = resend_rx.recv() => self.resend_evicted(&mut state, request, operator_address).await?,
                            changed = mined_floor_rx.changed() => changed.context("confirmation forwarder terminated")?,
                        }
                    }
                    let simulation_capacity = 256 - state.in_flight_prefix.len();
                    // Wait for window capacity first, then for commands: idle permits are
                    // never contended, and this order keeps commands in the channel (visible
                    // to backpressure) until they can actually be sent. Both waits service
                    // resend requests: the watcher may need a resend precisely when the
                    // window is full and no new sends can proceed.
                    state_reporter.enter_state(L1SenderState::WaitingL1Inclusion);
                    let first_permit = loop {
                        tokio::select! {
                            biased;
                            Some(request) = resend_rx.recv() => {
                                self.resend_evicted(&mut state, request, operator_address).await?;
                            }
                            permit = Arc::clone(&slots).acquire_owned() => {
                                break permit.expect("in-flight window semaphore is never closed");
                            }
                        }
                    };
                    let mut permits = vec![first_permit];
                    while permits.len() < window.min(simulation_capacity) {
                        match Arc::clone(&slots).try_acquire_owned() {
                            Ok(permit) => permits.push(permit),
                            Err(_) => break,
                        }
                    }

                    state_reporter.enter_state(L1SenderState::Idle);
                    let received = loop {
                        tokio::select! {
                            biased;
                            Some(request) = resend_rx.recv() => {
                                self.resend_evicted(&mut state, request, operator_address).await?;
                            }
                            received = inbound.recv_many(&mut raw_buffer, permits.len()) => {
                                break received;
                            }
                        }
                    };
                    if received == 0 {
                        tracing::info!(command_name, "inbound channel closed");
                        // Drop `submitted_tx` (by returning) so the inclusion watcher and the
                        // confirmation forwarder drain their queues and exit.
                        return anyhow::Ok(());
                    }
                    // Excess permits are released by dropping them.
                    permits.truncate(received);

                    let last = raw_buffer
                        .last()
                        .context("recv_many returned non-zero count but the buffer is empty")?;
                    state_reporter.record_picked(
                        last.last_block_number(),
                        last.block_timestamp(),
                        Some(last.last_batch_number()),
                    );
                    let commands = raw_buffer
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
                    self.submit_wave(
                        &mut state,
                        commands,
                        permits,
                        operator_address,
                        &submitted_tx,
                        PrefixProgressReceivers {
                            confirmed_floor: &mut mined_floor_rx,
                            resend: &mut resend_rx,
                        },
                    )
                    .await?;
                    L1_SENDER_METRICS.txs_in_flight[&command_name]
                        .set((window - slots.available_permits()) as u64);
                }
            }
        };

        let inclusion_watcher = {
            let slots = Arc::clone(&slots);
            let mined_tx = mined_tx;
            let resend_tx = resend_tx;
            async move {
                while let Some(mut entry) = submitted_rx.recv().await {
                    // Same-sender transactions mine in nonce order, so head-of-line sighting
                    // is exhaustive: this receipt appearing implies nothing behind it is
                    // mined-but-unobserved for long.
                    self.wait_for_inclusion(&mut entry, operator_address, &resend_tx)
                        .await?;
                    let InFlightTx {
                        nonce,
                        command,
                        submitted_at,
                        permit,
                        tx_hashes,
                        request,
                        fee_params: _,
                        raw_transactions,
                    } = entry;
                    // First sighting frees an unmined-window slot; confirmation depth is the
                    // forwarder's concern only.
                    drop(permit);
                    L1_SENDER_METRICS.txs_in_flight[&command_name]
                        .set((window - slots.available_permits()) as u64);
                    if mined_tx
                        .send(MinedTx {
                            tx_hashes,
                            nonce,
                            command,
                            submitted_at,
                            resend: ConfirmationResend {
                                request,
                                raw_transactions,
                            },
                        })
                        .await
                        .is_err()
                    {
                        // The forwarder only stops on error; `try_join!` surfaces its error,
                        // this just terminates the watcher promptly.
                        anyhow::bail!("confirmation forwarder terminated");
                    }
                }
                anyhow::Ok(())
            }
        };

        let confirmation_forwarder = async move {
            while let Some(mined) = mined_rx.recv().await {
                let receipt = self
                    .wait_for_confirmed_receipt(mined.tx_hashes, mined.resend)
                    .await;
                // Observe latency before propagating errors so timeout cases are recorded.
                L1_SENDER_METRICS.tx_inclusion_latency_seconds[&command_name]
                    .observe(mined.submitted_at.elapsed().as_secs_f64());
                let receipt = receipt?;
                self.validate_tx_receipt(&mined.command, receipt).await?;
                // A merely mined predecessor can disappear in a reorg; retain its simulation
                // prefix until this confirmation gate has checked the canonical block.
                mined_floor_tx.send_replace(mined.nonce + 1);
                // SYSCOIN: Retire durable wrapper ownership only after the same validated,
                // depth-confirmed receipt gate used for downstream pipeline progress.
                mined.command.notify_confirmed();
                tracing::info!(
                    command_name,
                    nonce = mined.nonce,
                    "L1 transaction confirmed, sending downstream",
                );
                for mut envelope in mined.command.into() {
                    envelope.set_stage(Input::MINED_STAGE);
                    outbound.send_and_record(envelope, state_reporter).await?;
                }
            }
            anyhow::Ok(())
        };

        // Boxed for stack safety, not style: these three state machines (the submitter
        // embeds the whole wave-submission and recovery-seeding machinery) otherwise live
        // inline in this future, which itself is moved by value when the pipeline spawns the
        // component — that spike overflowed an 8 MiB thread stack in debug builds.
        tokio::try_join!(
            Box::pin(submitter),
            Box::pin(inclusion_watcher),
            Box::pin(confirmation_forwarder)
        )?;
        Ok(())
    }

    /// Hands a previous session's recovered in-flight transactions to the tracking tasks.
    /// Each seed occupies an unmined-window permit (released as usual once observed mined) and
    /// gets a simulation-prefix entry so gas estimation for new commands chains correctly.
    async fn seed_recovered_txs(
        &self,
        seeds: Vec<RecoveredInFlight<Input>>,
        state: &mut SubmitterState,
        slots: &Arc<Semaphore>,
        submitted_tx: &mpsc::Sender<InFlightTx<Input>>,
        resend_rx: &mut mpsc::Receiver<ResendRequest>,
        operator_address: Address,
    ) -> anyhow::Result<()> {
        for seed in seeds {
            let request = self.build_tx_request(
                seed.command.solidity_call(self.gateway, &operator_address),
                operator_address,
                seed.nonce,
                seed.gas_limit,
                seed.fee_params,
            );
            let raw_transactions = if self.confirmation_liveness_enabled() {
                self.capture_raw_transaction(seed.tx_hash, &request)
                    .await?
                    .into_iter()
                    .map(|raw| (seed.tx_hash, raw))
                    .collect()
            } else {
                vec![]
            };
            state.in_flight_prefix.push_back(SimPrefixEntry {
                nonce: seed.nonce,
                calldata: seed.command.solidity_call(self.gateway, &operator_address),
                fee_params: seed.fee_params,
                gas_limit: seed.gas_limit,
            });
            self.note_submitted_batches(&seed.command);
            // If there are more seeds than window slots (the window was larger in a previous
            // session), this blocks until the inclusion watcher frees permits — it is already
            // running and needs no permits itself.
            let permit = loop {
                tokio::select! {
                    biased;
                    Some(request) = resend_rx.recv() => self.resend_evicted(state, request, operator_address).await?,
                    permit = Arc::clone(slots).acquire_owned() => break permit.expect("in-flight window semaphore is never closed"),
                }
            };
            submitted_tx
                .send(InFlightTx {
                    tx_hashes: vec![seed.tx_hash],
                    nonce: seed.nonce,
                    command: seed.command,
                    submitted_at: Instant::now(),
                    request,
                    fee_params: seed.fee_params,
                    raw_transactions,
                    permit,
                })
                .await
                .map_err(|_| anyhow::anyhow!("inclusion watcher terminated during seeding"))?;
        }
        Ok(())
    }

    /// Sends one wave of commands (bounded by the permits handed in), assigning sequential
    /// nonces and pushing each transaction to the inclusion watcher.
    async fn submit_wave(
        &self,
        state: &mut SubmitterState,
        commands: Vec<Input>,
        mut permits: Vec<OwnedSemaphorePermit>,
        operator_address: Address,
        submitted_tx: &mpsc::Sender<InFlightTx<Input>>,
        progress: PrefixProgressReceivers<'_>,
    ) -> anyhow::Result<()> {
        let command_name = Input::COMPONENT_ID.as_str();
        let range = Input::display_range(&commands);
        tracing::info!(command_name, range, "sending L1 transactions");
        L1_SENDER_METRICS.parallel_transactions[&command_name].set(commands.len() as u64);

        // Confirmed predecessors are part of the simulation base and no longer need replay.
        let (fee_params, gas_limits) = loop {
            let mined_floor = *progress.confirmed_floor.borrow_and_update();
            prune_confirmed_prefix(&mut state.in_flight_prefix, mined_floor);
            let fee_params = self
                .resolve_fee_params(
                    self.config.fee_config,
                    self.config.force_transaction_resubmission,
                )
                .await?;
            let fee_params = self.apply_replacement_floor(state, fee_params);
            match self
                .estimate_gas_limits(
                    state.in_flight_prefix.make_contiguous(),
                    &commands,
                    operator_address,
                    fee_params,
                    state.next_nonce,
                )
                .await
            {
                Ok(gas_limits) => break (fee_params, gas_limits),
                Err(error) if error.downcast_ref::<PendingSimulationPrefix>().is_some() => {
                    // Re-simulation is safe after a predecessor confirms; independent
                    // estimation before then could omit the state it supplies.
                    match wait_for_prefix_progress(progress.confirmed_floor, progress.resend)
                        .await?
                    {
                        PrefixProgress::Confirmed => {}
                        PrefixProgress::Resend(request) => {
                            self.resend_evicted(state, request, operator_address)
                                .await?;
                        }
                    }
                }
                Err(error) => return Err(error),
            }
        };
        tracing::info!(
            command_name,
            range,
            ?gas_limits,
            "estimated gas limits via eth_simulateV1",
        );
        for (mut command, gas_limit) in commands.into_iter().zip(gas_limits) {
            let nonce = state.next_nonce;
            let calldata = command.solidity_call(self.gateway, &operator_address);
            let tx_request = self.build_tx_request(
                calldata.clone(),
                operator_address,
                nonce,
                gas_limit,
                fee_params,
            );

            // Notify CommitWatcher before the transaction can possibly land on L1:
            // this batch number is being submitted by this session.
            self.note_submitted_batches(&command);

            let pending_tx = self
                .send_tx_with_retries(tx_request.clone(), &range)
                .await?;
            let tx_hash = *pending_tx.tx_hash();
            let raw_transactions = if self.confirmation_liveness_enabled() {
                self.capture_raw_transaction(tx_hash, &tx_request)
                    .await?
                    .into_iter()
                    .map(|raw| (tx_hash, raw))
                    .collect()
            } else {
                vec![]
            };
            state.next_nonce += 1;
            tracing::info!(
                "{command_name}: L1 transaction submitted for {range}. Hash: {tx_hash:?} (nonce {nonce})",
            );

            command
                .as_mut()
                .iter_mut()
                .for_each(|envelope| envelope.set_stage(Input::SENT_STAGE));
            state.in_flight_prefix.push_back(SimPrefixEntry {
                nonce,
                calldata,
                fee_params,
                gas_limit,
            });

            let permit = permits
                .pop()
                .context("fewer permits than commands in a wave")?;
            submitted_tx
                .send(InFlightTx {
                    tx_hashes: vec![tx_hash],
                    nonce,
                    command,
                    submitted_at: Instant::now(),
                    request: tx_request,
                    fee_params,
                    raw_transactions,
                    permit,
                })
                .await
                .map_err(|_| anyhow::anyhow!("inclusion watcher terminated"))?;
        }
        Ok(())
    }

    /// Applies (and expires) the recovery replacement-fee floor for the current wave. If any
    /// nonce in the wave replaces a stale transaction, the whole wave gets the floor — a
    /// bounded overpay that keeps fee resolution per-wave instead of per-tx.
    fn apply_replacement_floor(
        &self,
        state: &mut SubmitterState,
        fee_params: FeeParams,
    ) -> FeeParams {
        match state.replacement {
            Some(plan) if state.next_nonce < plan.until_nonce => {
                let raised = fee_params.apply_bounded_floor(
                    plan.fee_floor,
                    self.config
                        .fee_config
                        .fee_limits(self.config.force_transaction_resubmission),
                );
                tracing::warn!(
                    command_name = Input::COMPONENT_ID.as_str(),
                    until_nonce = plan.until_nonce,
                    next_nonce = state.next_nonce,
                    ?raised,
                    "replacing stale in-flight transactions from a previous session; \
                     applying a replacement fee floor within operator-selected fee limits",
                );
                raised
            }
            Some(_) => {
                state.replacement = None;
                fee_params
            }
            None => fee_params,
        }
    }

    /// Polls until one of the entry's candidate transactions has a receipt (any depth) and
    /// returns that hash. Never gives up on a transaction that is merely unmined — but a
    /// transaction *evicted from the pool* (fee spike, pool pressure) would otherwise be
    /// waited on forever, wedging the whole window, so eviction is detected and the
    /// transaction is resent at its nonce via the submitter.
    async fn wait_for_inclusion(
        &self,
        entry: &mut InFlightTx<Input>,
        operator_address: Address,
        resend_tx: &mpsc::Sender<ResendRequest>,
    ) -> anyhow::Result<B256> {
        let timeout = self.config.transaction_timeout;
        let poll_interval = self.config.poll_interval;
        let started_at = Instant::now();
        let mut next_warning_at = if timeout.is_zero() {
            None
        } else {
            Some(timeout)
        };
        let liveness_enabled = self.config.tx_liveness_max_missing_polls > 0
            && !self.config.tx_liveness_poll_interval.is_zero();
        let mut next_liveness_probe_at =
            liveness_enabled.then_some(self.config.tx_liveness_poll_interval);
        let mut consecutive_absent: u32 = 0;
        loop {
            for &tx_hash in &entry.tx_hashes {
                let receipt = self
                    .provider
                    .get_transaction_receipt(tx_hash)
                    .await
                    .with_context(|| {
                        format!("fetch receipt while waiting for L1 inclusion of tx {tx_hash}")
                    })?;
                if receipt.is_some() {
                    if self.confirmation_liveness_enabled()
                        && !entry
                            .raw_transactions
                            .iter()
                            .any(|(hash, _)| *hash == tx_hash)
                        && let Some(raw) = self
                            .capture_raw_transaction(tx_hash, &entry.request)
                            .await?
                    {
                        entry.raw_transactions.push((tx_hash, raw));
                    }
                    return Ok(tx_hash);
                }
            }

            let elapsed = started_at.elapsed();
            if let Some(probe_at) = next_liveness_probe_at
                && elapsed >= probe_at
            {
                if self.all_candidates_absent(entry).await? {
                    consecutive_absent += 1;
                } else {
                    consecutive_absent = 0;
                }
                if consecutive_absent >= self.config.tx_liveness_max_missing_polls {
                    self.handle_evicted_entry(entry, operator_address, resend_tx)
                        .await?;
                    consecutive_absent = 0;
                }
                next_liveness_probe_at = Some(elapsed + self.config.tx_liveness_poll_interval);
            }

            if let Some(warning_at) = next_warning_at
                && elapsed >= warning_at
            {
                tracing::warn!(
                    command_name = Input::COMPONENT_ID.as_str(),
                    tx_hashes = ?entry.tx_hashes,
                    nonce = entry.nonce,
                    waited_secs = elapsed.as_secs_f64(),
                    "still waiting for L1 inclusion; if the transaction is stuck underpriced, \
                     restart with `force_transaction_resubmission.enabled = true`",
                );
                next_warning_at = Some(warning_at + timeout);
            }
            tokio::time::sleep(poll_interval).await;
        }
    }

    /// True when none of the entry's candidate transactions are known to the L1 node —
    /// neither pooled nor mined.
    async fn all_candidates_absent(&self, entry: &InFlightTx<Input>) -> anyhow::Result<bool> {
        for &tx_hash in &entry.tx_hashes {
            let known = self
                .provider
                .get_transaction_by_hash(tx_hash)
                .await
                .with_context(|| format!("probe pool presence of tx {tx_hash}"))?
                .is_some();
            if known {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// All candidates for this nonce vanished from the pool. If the nonce was consumed by a
    /// transaction that is none of ours, another sender is using the operator account — fatal.
    /// Otherwise the transaction was evicted (fee spike, pool pressure): ask the submitter to
    /// resend it at the same nonce and track the new hash alongside the old candidates (the
    /// eviction is observed remotely, so an old candidate may still mine elsewhere).
    async fn handle_evicted_entry(
        &self,
        entry: &mut InFlightTx<Input>,
        operator_address: Address,
        resend_tx: &mpsc::Sender<ResendRequest>,
    ) -> anyhow::Result<()> {
        let latest_nonce = self
            .provider
            .get_transaction_count(operator_address)
            .await
            .context("get confirmed nonce while handling a pool eviction")?;
        if latest_nonce > entry.nonce {
            // Give in-flight receipt propagation one more chance before declaring foul play.
            for &tx_hash in &entry.tx_hashes {
                if self
                    .provider
                    .get_transaction_receipt(tx_hash)
                    .await?
                    .is_some()
                {
                    return Ok(());
                }
            }
            anyhow::bail!(
                "nonce {} was consumed by a transaction that is not one of ours ({:?}); \
                 the operator account is being used by another sender",
                entry.nonce,
                entry.tx_hashes,
            );
        }

        tracing::warn!(
            command_name = Input::COMPONENT_ID.as_str(),
            nonce = entry.nonce,
            tx_hashes = ?entry.tx_hashes,
            "in-flight transaction was evicted from the L1 pool; resending at the same nonce",
        );
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        resend_tx
            .send(ResendRequest {
                nonce: entry.nonce,
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("submitter terminated during an eviction resend"))?;
        let (new_hash, fees, raw) = reply_rx
            .await
            .context("submitter dropped an eviction resend request")?;
        if !entry.tx_hashes.contains(&new_hash) {
            entry.tx_hashes.push(new_hash);
        }
        entry.fee_params = fees;
        entry.request.max_fee_per_gas = Some(fees.max_fee_per_gas);
        entry.request.max_priority_fee_per_gas = Some(fees.max_priority_fee_per_gas);
        if let Some(raw) = raw {
            entry.raw_transactions.push((new_hash, raw));
        }
        entry.submitted_at = Instant::now();
        Ok(())
    }

    /// Rebuilds and resends the transaction at `request.nonce` from its simulation-prefix
    /// entry after a pool eviction. Fees are re-resolved and floored at the original send's
    /// fees plus the pool's replacement bump, in case the evicted transaction resurfaces.
    async fn resend_evicted(
        &self,
        state: &mut SubmitterState,
        request: ResendRequest,
        operator_address: Address,
    ) -> anyhow::Result<()> {
        let entry = state
            .in_flight_prefix
            .iter_mut()
            .find(|entry| entry.nonce == request.nonce)
            .with_context(|| {
                format!(
                    "no in-flight prefix entry for evicted nonce {}",
                    request.nonce
                )
            })?;

        let resolved = self
            .resolve_fee_params(
                self.config.fee_config,
                self.config.force_transaction_resubmission,
            )
            .await?;
        // SYSCOIN: A remote RPC can falsely report an in-flight transaction as evicted; preserve
        // the replacement attempt without allowing that signal to bypass operator fee limits.
        let fee_params = resolved.apply_bounded_floor(
            entry.fee_params.replacement_floor(),
            self.config
                .fee_config
                .fee_limits(self.config.force_transaction_resubmission),
        );
        let tx_request = self.build_tx_request(
            entry.calldata.clone(),
            operator_address,
            entry.nonce,
            entry.gas_limit,
            fee_params,
        );
        let pending_tx = self
            .send_tx_with_retries(
                tx_request.clone(),
                &format!("resend of nonce {}", request.nonce),
            )
            .await?;
        entry.fee_params = fee_params;

        let new_hash = *pending_tx.tx_hash();
        let raw = if self.confirmation_liveness_enabled() {
            self.capture_raw_transaction(new_hash, &tx_request).await?
        } else {
            None
        };
        tracing::info!(
            command_name = Input::COMPONENT_ID.as_str(),
            nonce = request.nonce,
            tx_hash = ?new_hash,
            "resent evicted L1 transaction",
        );
        // The watcher owns the entry lifecycle; if it is gone the try_join surfaces its error.
        let _ = request.reply.send((new_hash, fee_params, raw));
        Ok(())
    }

    /// Detects in-flight L1 transactions from a previous session and derives the pipelined
    /// startup state: which transactions to track as already-submitted, which nonce to
    /// continue from, and whether a stale suffix must be replaced with bumped fees.
    ///
    /// Pairing: for each pending nonce the on-chain transaction's calldata is compared
    /// against the next queued command. SYSCOIN: this is pinned to the same settlement-layer
    /// `sl_block_number` at which the inbound queue was constructed because the provider may be
    /// Gateway rather than L1. A match is tracked as already-submitted; a mismatch or dropped
    /// transaction produces a [`ReplacementPlan`] so the stale suffix is replaced at its own
    /// nonces instead of appending duplicates behind it.
    ///
    /// Returns `None` when the inbound channel closes mid-recovery.
    async fn plan_pipelined_recovery(
        &self,
        inbound: &mut PeekableReceiver<L1SenderCommand<Input>>,
        state_reporter: &ComponentStateReporter,
        operator_address: Address,
        latest_nonce: u64,
    ) -> anyhow::Result<Option<PipelinedStart<Input>>> {
        let command_name = Input::COMPONENT_ID.as_str();
        let pending_nonce = self
            .provider
            .get_transaction_count(operator_address)
            .pending()
            .await
            .context("get pending transaction count")?;

        if self.config.force_transaction_resubmission {
            if pending_nonce > latest_nonce {
                tracing::warn!(
                    command_name,
                    latest_nonce,
                    pending_nonce,
                    "force resubmission: replacing {} in-flight transactions starting from the \
                     confirmed nonce with replacement fees",
                    pending_nonce - latest_nonce,
                );
            }
            // Replacement fees are applied session-wide via `resolve_fee_params`.
            return Ok(Some(PipelinedStart {
                seeds: vec![],
                next_nonce: latest_nonce,
                mined_floor: latest_nonce,
                replacement: None,
            }));
        }

        if pending_nonce <= latest_nonce {
            return Ok(Some(PipelinedStart {
                seeds: vec![],
                next_nonce: pending_nonce.max(latest_nonce),
                mined_floor: latest_nonce,
                replacement: None,
            }));
        }

        let chain_id = self
            .provider
            .get_chain_id()
            .await
            .context("get settlement chain id during recovery")?;

        let in_flight_count = (pending_nonce - latest_nonce) as usize;
        tracing::info!(
            command_name,
            sl_block_number = self.sl_block_number,
            latest_nonce,
            pending_nonce,
            in_flight_count,
            "Detected in-flight L1 transactions on startup, attempting recovery",
        );

        // Probe whether the provider supports `eth_getTransactionBySenderAndNonce` before
        // iterating over all pending nonces.
        if let Err(TransportError::ErrorResp(ref e)) = self
            .provider
            .get_transaction_by_sender_nonce(operator_address, latest_nonce)
            .await
        {
            if e.code == METHOD_NOT_FOUND_CODE {
                tracing::warn!(
                    command_name,
                    in_flight_count,
                    "eth_getTransactionBySenderAndNonce is not supported by the current L1 \
                     provider, so in-flight transactions cannot be paired with queued commands. \
                     Waiting for them to settle before sending anything. For crash-tolerant \
                     operation use a provider that supports the method (reth/Erigon family), or \
                     restart with `force_transaction_resubmission.enabled = true` to replace \
                     them.",
                );
                self.wait_for_in_flight_txs_to_settle(operator_address)
                    .await?;
                anyhow::bail!(
                    "pending transactions settled; restart to rebuild commands from the canonical frontier"
                );
            }
            anyhow::bail!("Error while probing eth_getTransactionBySenderAndNonce support: {e}");
        }

        let mut seeds: Vec<RecoveredInFlight<Input>> = Vec::with_capacity(in_flight_count);
        let mut replacement = None;
        for nonce in latest_nonce..pending_nonce {
            let tx = match self
                .provider
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
                        "In-flight transaction at nonce {nonce} was dropped from the mempool; \
                         re-sending queued commands from that nonce",
                    );
                    replacement = self
                        .build_replacement_plan(
                            operator_address,
                            chain_id,
                            nonce,
                            pending_nonce,
                            None,
                        )
                        .await?;
                    break;
                }
            };

            anyhow::ensure!(
                tx.from() == operator_address
                    && ConsensusTransaction::nonce(&tx) == nonce
                    && ConsensusTransaction::to(&tx) == Some(self.to_address)
                    && ConsensusTransaction::value(&tx).is_zero()
                    && ConsensusTransaction::chain_id(&tx) == Some(chain_id),
                "foreign in-flight transaction at sender nonce"
            );

            // Peek at the next command without consuming it so that a mismatch leaves
            // `inbound` intact for the normal send path.
            let matches = inbound
                .peek_recv(|raw_cmd| {
                    let L1SenderCommand::SendToL1(cmd) = raw_cmd else {
                        return false;
                    };
                    cmd.solidity_call(self.gateway, &operator_address) == *tx.input()
                })
                .await;

            match matches {
                None => return Ok(None),
                Some(false) => {
                    tracing::warn!(
                        command_name,
                        nonce,
                        "In-flight transaction calldata does not match the next queued command; \
                         replacing in-flight transactions from nonce {nonce} with queued \
                         commands at bumped fees",
                    );
                    replacement = self
                        .build_replacement_plan(
                            operator_address,
                            chain_id,
                            nonce,
                            pending_nonce,
                            Some(&tx),
                        )
                        .await?;
                    break;
                }
                Some(true) => {
                    let Some(L1SenderCommand::SendToL1(cmd)) =
                        inbound.recv_and_record_picked(state_reporter).await
                    else {
                        unreachable!("peek succeeded, recv must return the same item");
                    };
                    seeds.push(RecoveredInFlight {
                        tx_hash: tx.tx_hash(),
                        nonce,
                        command: cmd,
                        fee_params: fee_params_of_tx(&tx),
                        gas_limit: ConsensusTransaction::gas_limit(&tx),
                    });
                }
            }
        }

        let next_nonce = latest_nonce + seeds.len() as u64;
        tracing::info!(
            command_name,
            recovered = seeds.len(),
            in_flight_count,
            next_nonce,
            replacement_active = replacement.is_some(),
            "Recovered in-flight transactions; tracking their inclusion in the background",
        );

        Ok(Some(PipelinedStart {
            seeds,
            next_nonce,
            mined_floor: latest_nonce,
            replacement,
        }))
    }

    /// Computes the replacement-fee floor for a stale in-flight suffix `[from_nonce,
    /// until_nonce)`: the per-field maximum of every still-visible stale transaction, bumped
    /// by the regular transaction-pool rule. Returns `None` when no stale transaction is
    /// visible anymore (nothing to outbid).
    /// The operator account must be exclusive to this sender: different calldata sent to the
    /// same target cannot be distinguished from a stale command in the suffix.
    pub(super) async fn build_replacement_plan(
        &self,
        operator_address: Address,
        chain_id: u64,
        from_nonce: u64,
        until_nonce: u64,
        first_stale: Option<&L1TxResponse>,
    ) -> anyhow::Result<Option<ReplacementPlan>> {
        let mut stale_fee_max: Option<FeeParams> = None;
        let mut fold = |tx: &L1TxResponse| {
            let fees = fee_params_of_tx(tx);
            stale_fee_max = Some(match stale_fee_max {
                Some(current) => current.max(fees),
                None => fees,
            });
        };
        if let Some(tx) = first_stale {
            fold(tx);
        }
        let already_fetched = first_stale.is_some() as u64;
        for nonce in (from_nonce + already_fetched)..until_nonce {
            match self
                .provider
                .get_transaction_by_sender_nonce(operator_address, nonce)
                .await
            {
                Ok(Some(tx)) => {
                    anyhow::ensure!(
                        tx.from() == operator_address
                            && ConsensusTransaction::nonce(&tx) == nonce
                            && ConsensusTransaction::to(&tx) == Some(self.to_address)
                            && ConsensusTransaction::value(&tx).is_zero()
                            && ConsensusTransaction::chain_id(&tx) == Some(chain_id),
                        "foreign in-flight transaction in replacement suffix at nonce {nonce}"
                    );
                    fold(&tx);
                }
                Ok(None) => {}
                Err(err) => {
                    anyhow::bail!("failed to inspect replacement suffix at nonce {nonce}: {err}");
                }
            }
        }

        Ok(stale_fee_max.map(|fees| ReplacementPlan {
            until_nonce,
            fee_floor: fees.replacement_floor(),
        }))
    }

    /// Polls until the operator account has no pending transactions (pending nonce == latest
    /// nonce). Used when in-flight transactions exist but cannot be paired with queued
    /// commands because the provider lacks `eth_getTransactionBySenderAndNonce`.
    pub(crate) async fn wait_for_in_flight_txs_to_settle(
        &self,
        operator_address: Address,
    ) -> anyhow::Result<u64> {
        let timeout = self.config.transaction_timeout;
        let poll_interval = self.config.poll_interval;
        let started_at = Instant::now();
        let mut next_warning_at = if timeout.is_zero() {
            None
        } else {
            Some(timeout)
        };
        loop {
            let latest_nonce = self
                .provider
                .get_transaction_count(operator_address)
                .await
                .context(
                    "get latest transaction count while waiting for in-flight txs to settle",
                )?;
            let pending_nonce = self
                .provider
                .get_transaction_count(operator_address)
                .pending()
                .await
                .context(
                    "get pending transaction count while waiting for in-flight txs to settle",
                )?;
            if pending_nonce <= latest_nonce {
                return Ok(latest_nonce);
            }

            let elapsed = started_at.elapsed();
            if let Some(warning_at) = next_warning_at
                && elapsed >= warning_at
            {
                tracing::warn!(
                    command_name = Input::COMPONENT_ID.as_str(),
                    latest_nonce,
                    pending_nonce,
                    waited_secs = elapsed.as_secs_f64(),
                    "still waiting for previous-session in-flight transactions to settle; \
                     restart with `force_transaction_resubmission.enabled = true` to replace \
                     them instead",
                );
                next_warning_at = Some(warning_at + timeout);
            }
            tokio::time::sleep(poll_interval).await;
        }
    }
}

/// Extracts the EIP-1559 fee parameters a transaction was actually sent with.
pub(crate) fn fee_params_of_tx(tx: &L1TxResponse) -> FeeParams {
    FeeParams {
        max_fee_per_gas: ConsensusTransaction::max_fee_per_gas(tx),
        max_priority_fee_per_gas: ConsensusTransaction::max_priority_fee_per_gas(tx)
            .unwrap_or_default(),
    }
}
