use super::proof_storage::ProofStorage;
use super::prover_job_map::StartupRecoveryPlan;
use super::snark_job_manager::SnarkJobManager;
use super::snark_proof_journal::{SnarkProofJournal, validate_batch_against_committed};
use super::snark_proof_preflight::{OnchainSnarkProofPreflight, SnarkProofPreflight};
use crate::prover_api::fri_proof_verifier;
use alloy::eips::{BlockId, BlockNumberOrTag};
use alloy::primitives::{Address, B256};
use alloy::providers::Provider as _;
use anyhow::Context as _;
use async_trait::async_trait;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use zksync_os_batch_types::batcher_model::{FriProof, SignedBatchEnvelope};
use zksync_os_contract_interface::ZkChain;
use zksync_os_l1_sender::commands::L1SenderCommand;
use zksync_os_l1_sender::commands::prove::ProofCommand;
use zksync_os_l1_sender::config::ConfirmationPolicy;
use zksync_os_l1_watcher::CommittedBatchProvider;
use zksync_os_observability::ComponentStateReporter;
use zksync_os_pipeline::{PeekableReceiver, PipelineComponent, SendAndRecordExt};
use zksync_os_provider::NodeProvider;
use zksync_os_types::{ProtocolSemanticVersion, ProvingVersion};

/// Pipeline step that waits for batches to be SNARK proved.
///
/// This component:
/// - Receives batches with FRI proofs (after they are committed to L1)
/// - Forwards them to SnarkJobManager (which makes them available via HTTP API)
/// - Receives batches with proofs from SnarkJobManager (submitted via HTTP API or fake provers)
/// - Forwards the proof commands downstream to L1 proof sender
///
/// The SnarkJobManager itself is purely reactive (no run loop), accessed/driven by:
/// - HTTP server (provers call pick_next_job, submit_proof, etc.)
/// - Fake provers pool
pub struct SnarkProvingPipelineStep {
    // SYSCOIN: The recreated pipeline begins exactly after the executed frontier, including
    // already-proved passthrough markers before the first unproved FRI.
    last_executed_batch_number: u64,
    last_proved_batch_number: u64,
    last_committed_batch_number: u64,
    // SYSCOIN: Recovery partitions the durable backlog using the same wrapper and resident-span
    // limits supplied to the live manager, so planned heads are always admissible and wrappable.
    max_fris_per_snark: usize,
    max_assigned_batch_range: usize,
    proof_storage: ProofStorage,
    committed_batch_provider: CommittedBatchProvider,
    snark_job_manager: Arc<SnarkJobManager>,
    // SYSCOIN: Startup replay and live HTTP admission share the identical on-chain verifier
    // preflight, preventing a crash from bypassing the original acceptance boundary.
    preflight: Arc<dyn SnarkProofPreflight>,
    proof_commands_receiver: mpsc::Receiver<ProofCommand>,
    // SYSCOIN: Own startup replay and the sole confirmation receiver for durable wrappers.
    journal: SnarkProofJournal,
    journal_confirmations: mpsc::UnboundedReceiver<String>,
    chain_id: u64,
    chain_address: Address,
    // SYSCOIN: Startup cleanup uses the same provider and inclusive confirmation policy as the
    // selected prove sender; latest-only state can never retire the sole durable wrapper.
    sl_provider: NodeProvider,
    sl_chain_id: u64,
    confirmation_policy: ConfirmationPolicy,
    // SYSCOIN: Persist the startup-selected real/mock lane across live and restart ingestion.
    proof_mode: SnarkFriProofMode,
    // SYSCOIN: The SNARK stage exclusively owns this monotonic liveness lease. Prover serving may
    // begin at Drainable, while public node readiness remains closed until Ready.
    startup_phase: watch::Sender<SnarkStartupPhase>,
}

/// SYSCOIN: Separate authenticated prover draining from public node readiness without allowing two
/// independent booleans to drift. The stage is the sole sender and may advance only in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnarkStartupPhase {
    Recovering,
    Drainable,
    Ready,
}

impl SnarkStartupPhase {
    /// SYSCOIN: Keep phase comparisons explicit rather than relying on enum declaration order.
    pub(crate) const fn satisfies(self, required: Self) -> bool {
        matches!(
            (self, required),
            (Self::Recovering, Self::Recovering)
                | (Self::Drainable, Self::Recovering | Self::Drainable)
                | (Self::Ready, _)
        )
    }

    // SYSCOIN: The trusted stage sender still asserts every transition so receiver-side watch
    // coalescing can never conceal a regression produced inside this process.
    const fn can_advance_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Recovering, Self::Drainable) | (Self::Drainable, Self::Ready)
        )
    }
}

// SYSCOIN: Centralize the only production phase publication and fail immediately on a duplicate,
// skipped, or regressing transition.
fn advance_startup_phase(sender: &watch::Sender<SnarkStartupPhase>, next: SnarkStartupPhase) {
    let previous = *sender.borrow();
    assert!(
        previous.can_advance_to(next),
        "invalid SNARK startup phase transition {previous:?} -> {next:?}"
    );
    let replaced = sender.send_replace(next);
    assert_eq!(
        replaced, previous,
        "SNARK startup phase changed outside its stage-owned transition"
    );
}

// SYSCOIN: Pipeline shutdown drops the component future from an outer select. Tokio join handles
// detach on drop, so retain explicit abort guards for the proof forwarder and journal reaper.
struct AbortTaskOnDrop(tokio::task::AbortHandle);

impl Drop for AbortTaskOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

// SYSCOIN: A fresh V32 node must never reinterpret durable proof variants after changing the
// deployed verifier mode; the explicit mode excludes passthrough markers from both lanes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnarkFriProofMode {
    Real,
    Fake,
}

impl SnarkFriProofMode {
    fn accepts(self, proof: &FriProof) -> bool {
        matches!(
            (self, proof),
            (Self::Real, FriProof::Real(_)) | (Self::Fake, FriProof::Fake)
        )
    }

    fn expected_kind(self) -> &'static str {
        match self {
            Self::Real => "real",
            Self::Fake => "fake",
        }
    }
}

fn fri_proof_kind(proof: &FriProof) -> &'static str {
    match proof {
        FriProof::Real(_) => "real",
        FriProof::Fake => "fake",
        FriProof::AlreadySubmittedToL1 => "already-submitted-to-l1 marker",
    }
}

impl SnarkProvingPipelineStep {
    // SYSCOIN: Keep startup-discovered proof frontiers, settlement identity, verifier mode, and
    // the journal receiver explicit at this one construction boundary; collapsing them into an
    // unvalidated bag would obscure which values are bound during durable recovery.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        proof_storage: ProofStorage,
        // SYSCOIN: Configure the two-proof, target-or-age aggregation policy end to end.
        max_fris_per_snark: usize,
        target_fris_per_snark: usize,
        max_snark_batch_wait: Duration,
        last_executed_batch_number: u64,
        last_proved_batch_number: u64,
        last_committed_batch_number: u64,
        assignment_timeout: Duration,
        max_assigned_batch_range: usize,
        committed_batch_provider: CommittedBatchProvider,
        journal: SnarkProofJournal,
        journal_confirmations: mpsc::UnboundedReceiver<String>,
        chain_id: u64,
        chain_address: Address,
        sl_provider: NodeProvider,
        sl_chain_id: u64,
        required_confirmations: u64,
        expected_vk_hash: B256,
        // SYSCOIN: Startup verifier policy already enforces an all-real or all-fake topology.
        fake_proving: bool,
    ) -> (
        Self,
        Arc<SnarkJobManager>,
        watch::Receiver<SnarkStartupPhase>,
    ) {
        let (proof_commands_sender, proof_commands_receiver) = mpsc::channel::<ProofCommand>(1);
        let (startup_phase, startup_phase_receiver) = watch::channel(SnarkStartupPhase::Recovering);

        let preflight: Arc<dyn SnarkProofPreflight> = Arc::new(OnchainSnarkProofPreflight::new(
            sl_provider.clone(),
            chain_address,
            sl_chain_id,
            expected_vk_hash,
        ));
        let snark_job_manager = Arc::new(SnarkJobManager::new_with_journal(
            proof_commands_sender,
            max_fris_per_snark,
            target_fris_per_snark,
            max_snark_batch_wait,
            assignment_timeout,
            max_assigned_batch_range,
            journal.clone(),
            preflight.clone(),
        ));

        let result = Self {
            last_executed_batch_number,
            last_proved_batch_number,
            last_committed_batch_number,
            max_fris_per_snark,
            max_assigned_batch_range,
            proof_storage,
            committed_batch_provider,
            snark_job_manager: snark_job_manager.clone(),
            preflight,
            proof_commands_receiver,
            journal,
            journal_confirmations,
            chain_id,
            chain_address,
            sl_provider,
            sl_chain_id,
            confirmation_policy: ConfirmationPolicy::new(required_confirmations),
            // SYSCOIN: Bind restart recovery to the startup-selected verifier/prover lane so a
            // stored mock proof can never cross a later production-verifier transition.
            proof_mode: if fake_proving {
                SnarkFriProofMode::Fake
            } else {
                SnarkFriProofMode::Real
            },
            startup_phase,
        };

        (result, snark_job_manager, startup_phase_receiver)
    }
}

// SYSCOIN: The startup cursor accepts only the gapless stream produced after the canonical
// executed frontier. Disk recovery may run ahead of this cursor, but every recreated duplicate is
// still validated in exact numeric order before it can be discarded.
struct OrderedSnarkInputCursor {
    next_expected: u64,
}

impl OrderedSnarkInputCursor {
    fn after(last_executed_batch_number: u64) -> anyhow::Result<Self> {
        Ok(Self {
            next_expected: last_executed_batch_number
                .checked_add(1)
                .context("SNARK input cursor overflow after executed frontier")?,
        })
    }

    async fn recv_next(
        &mut self,
        input: &mut PeekableReceiver<SignedBatchEnvelope<FriProof>>,
        state_reporter: &ComponentStateReporter,
    ) -> anyhow::Result<Option<SignedBatchEnvelope<FriProof>>> {
        let Some(batch) = input.recv_and_record_picked(state_reporter).await else {
            return Ok(None);
        };
        let actual = batch.batch_number();
        anyhow::ensure!(
            actual == self.next_expected,
            "SNARK input is not gapless: expected batch {}, got {actual}",
            self.next_expected
        );
        self.next_expected = self
            .next_expected
            .checked_add(1)
            .context("SNARK input cursor overflow")?;
        Ok(Some(batch))
    }
}

#[derive(Clone, Copy)]
enum ExpectedFriKind {
    Passthrough,
    Proved(SnarkFriProofMode),
}

// SYSCOIN: CommittedBatchProvider does not yet expose canonical protocol boundaries. Fresh V32
// recovery is therefore deliberately homogeneous V8; a later server pin must enrich discovery
// before this guard may change, rather than inferring a version from evictable local proof files.
fn canonical_startup_proving_version() -> anyhow::Result<ProvingVersion> {
    let protocol = ProtocolSemanticVersion::canonical_genesis_version();
    anyhow::ensure!(
        protocol == ProtocolSemanticVersion::new(0, 32, 0),
        "startup SNARK recovery requires version-aware committed metadata beyond V32"
    );
    let proving_version = ProvingVersion::try_from(protocol)
        .context("resolve canonical startup SNARK proving version")?;
    anyhow::ensure!(
        proving_version == ProvingVersion::V8,
        "V32 startup SNARK recovery must use proving version V8"
    );
    Ok(proving_version)
}

// SYSCOIN: Bounded recovery can await map capacity without reading its upstream edge. Poll sender
// liveness during that wait so a dead recreated pipeline cannot leave the node indefinitely in a
// superficially live Drainable phase.
async fn wait_for_snark_input_close(input: &PeekableReceiver<SignedBatchEnvelope<FriProof>>) {
    while !input.is_closed() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// SYSCOIN: A settlement refresh may include both a confirmation delay and slow RPC calls. Keep
// both pipeline edges in the same select so a dead upstream or downstream cannot strand public
// readiness forever in Recovering while the durable journal remains safely on disk.
async fn await_startup_confirmation_refresh<T, F>(
    input: &PeekableReceiver<SignedBatchEnvelope<FriProof>>,
    output: &mpsc::Sender<L1SenderCommand<ProofCommand>>,
    refresh: F,
) -> anyhow::Result<T>
where
    F: Future<Output = anyhow::Result<T>>,
{
    tokio::select! {
        result = refresh => result,
        () = wait_for_snark_input_close(input) => {
            anyhow::bail!(
                "SNARK pipeline inbound channel closed while awaiting startup journal confirmation depth"
            );
        }
        _ = output.closed() => {
            anyhow::bail!(
                "SNARK pipeline outbound channel closed while awaiting startup journal confirmation depth"
            );
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CanonicalProvedFrontiers {
    tip_block: u64,
    safe_block: u64,
    latest_proved: u64,
    confirmation_safe_proved: u64,
}

// SYSCOIN: Bind latest and confirmation-safe proved counters to canonical EIP-1898 hashes from
// one SL identity. Numbered postchecks make a concurrent reorg/topology change restart the full
// node discovery path instead of deleting or replaying a wrapper against an ambiguous snapshot.
async fn canonical_proved_frontiers(
    provider: &NodeProvider,
    diamond_address: Address,
    expected_sl_chain_id: u64,
    confirmation_policy: ConfirmationPolicy,
) -> anyhow::Result<CanonicalProvedFrontiers> {
    let chain_id_before = provider.get_chain_id().await?;
    anyhow::ensure!(
        chain_id_before == expected_sl_chain_id,
        "settlement-layer chain ID changed before SNARK journal recovery"
    );

    let tip = provider
        .get_block_by_number(BlockNumberOrTag::Latest)
        .await?
        .context("settlement layer returned no latest block")?;
    let tip_block = tip.header.inner.number;
    let tip_hash = tip.header.hash;
    let safe_block = confirmation_policy.safe_state_block(tip_block);
    let safe = if safe_block == tip_block {
        tip.clone()
    } else {
        provider
            .get_block_by_number(BlockNumberOrTag::Number(safe_block))
            .await?
            .with_context(|| {
                format!("settlement layer returned no confirmation-safe block {safe_block}")
            })?
    };
    anyhow::ensure!(
        safe.header.inner.number == safe_block,
        "settlement layer returned the wrong confirmation-safe block"
    );
    let safe_hash = safe.header.hash;
    let diamond = ZkChain::new(diamond_address, provider.clone());
    let latest_proved = diamond
        .get_total_batches_proved(BlockId::hash_canonical(tip_hash))
        .await?;
    let confirmation_safe_proved = diamond
        .get_total_batches_proved(BlockId::hash_canonical(safe_hash))
        .await?;
    anyhow::ensure!(
        confirmation_safe_proved <= latest_proved,
        "confirmation-safe proved frontier is ahead of latest proved frontier"
    );

    let canonical_tip = provider
        .get_block_by_number(BlockNumberOrTag::Number(tip_block))
        .await?
        .context("settlement-layer tip disappeared during SNARK journal recovery")?;
    let canonical_safe = if safe_block == tip_block {
        canonical_tip.clone()
    } else {
        provider
            .get_block_by_number(BlockNumberOrTag::Number(safe_block))
            .await?
            .context("confirmation-safe block disappeared during SNARK journal recovery")?
    };
    anyhow::ensure!(
        canonical_tip.header.hash == tip_hash && canonical_safe.header.hash == safe_hash,
        "settlement-layer reorged during SNARK journal recovery"
    );
    let chain_id_after = provider.get_chain_id().await?;
    anyhow::ensure!(
        chain_id_after == expected_sl_chain_id,
        "settlement-layer chain ID changed during SNARK journal recovery"
    );

    Ok(CanonicalProvedFrontiers {
        tip_block,
        safe_block,
        latest_proved,
        confirmation_safe_proved,
    })
}

// SYSCOIN: Recovery and FRI rehydration are classified against the startup discovery frontier.
// Any concurrent prover movement therefore requires a full restart, even when it moves forward;
// only the confirmation-safe historical frontier may advance while covered records are retained.
fn ensure_startup_proved_frontier_unchanged(
    startup_proved: u64,
    refreshed_proved: u64,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        refreshed_proved == startup_proved,
        "settlement-layer proved frontier changed while awaiting durable SNARK journal confirmation; restart full L1 discovery"
    );
    Ok(())
}

// SYSCOIN: Canonical validation precedes both queue admission and completed-ownership checks.
// A range tombstone can suppress duplicate work only after the incoming bytes independently match
// the V32 committed batch, execute root, chain identity, protocol, and selected FRI lane.
impl SnarkProvingPipelineStep {
    async fn validate_canonical_snark_fri(
        committed_batch_provider: &CommittedBatchProvider,
        expected_batch_number: u64,
        chain_id: u64,
        chain_address: Address,
        expected_kind: ExpectedFriKind,
        batch: &SignedBatchEnvelope<FriProof>,
    ) -> anyhow::Result<()> {
        validate_batch_against_committed(
            &batch.batch,
            expected_batch_number,
            chain_id,
            chain_address,
            committed_batch_provider,
        )
        .await?;

        match expected_kind {
            ExpectedFriKind::Passthrough => anyhow::ensure!(
                matches!(batch.data, FriProof::AlreadySubmittedToL1),
                "SNARK passthrough batch {expected_batch_number} has {}, expected already-submitted-to-l1 marker",
                fri_proof_kind(&batch.data)
            ),
            ExpectedFriKind::Proved(proof_mode) => {
                anyhow::ensure!(
                    proof_mode.accepts(&batch.data),
                    "SNARK input proof mode mismatch for batch {expected_batch_number}: expected {}, got {}",
                    proof_mode.expected_kind(),
                    fri_proof_kind(&batch.data),
                );
                if let FriProof::Real(real) = &batch.data {
                    // SYSCOIN: Re-verify durable and recreated real FRI bytes before an external
                    // wrapper can ever receive them; committed metadata alone does not prove FRI.
                    fri_proof_verifier::verify_real_fri_proof_bytes(&batch.batch, real.proof())
                        .with_context(|| {
                            format!("invalid real FRI proof for batch {expected_batch_number}")
                        })?;
                }
            }
        }
        Ok(())
    }

    // SYSCOIN: When a local FRI is absent or corrupt, consume only the exact gapless recreated
    // batch. Earlier proved markers are forwarded, while canonically valid disk/journal duplicates
    // are discarded without resetting queue age or creating a second wrapper.
    #[allow(clippy::too_many_arguments)]
    async fn recv_recreated_fri_for(
        input: &mut PeekableReceiver<SignedBatchEnvelope<FriProof>>,
        cursor: &mut OrderedSnarkInputCursor,
        target_batch_number: u64,
        last_proved_batch_number: u64,
        committed_batch_provider: &CommittedBatchProvider,
        chain_id: u64,
        chain_address: Address,
        proof_mode: SnarkFriProofMode,
        output: &mpsc::Sender<L1SenderCommand<ProofCommand>>,
        state_reporter: &ComponentStateReporter,
    ) -> anyhow::Result<SignedBatchEnvelope<FriProof>> {
        anyhow::ensure!(
            cursor.next_expected <= target_batch_number,
            "SNARK recovery cursor already passed missing batch {target_batch_number}"
        );
        loop {
            let batch = cursor
                .recv_next(input, state_reporter)
                .await?
                .context("SNARK pipeline inbound channel closed during startup recovery")?;
            let batch_number = batch.batch_number();
            if batch_number <= last_proved_batch_number {
                Self::validate_canonical_snark_fri(
                    committed_batch_provider,
                    batch_number,
                    chain_id,
                    chain_address,
                    ExpectedFriKind::Passthrough,
                    &batch,
                )
                .await?;
                output
                    .send_and_record(
                        L1SenderCommand::Passthrough(Box::new(batch)),
                        state_reporter,
                    )
                    .await?;
                continue;
            }

            anyhow::ensure!(
                batch_number <= target_batch_number,
                "SNARK recovery skipped missing batch {target_batch_number} and received {batch_number}"
            );
            Self::validate_canonical_snark_fri(
                committed_batch_provider,
                batch_number,
                chain_id,
                chain_address,
                ExpectedFriKind::Proved(proof_mode),
                &batch,
            )
            .await?;
            if batch_number == target_batch_number {
                return Ok(batch);
            }

            tracing::info!(
                batch_number,
                target_batch_number,
                "discarding canonically validated recreated FRI already classified during startup"
            );
        }
    }

    // SYSCOIN: Classify the startup range strictly in ascending order. The prover listener opens
    // first so bounded blocking admission can drain; disk remains the overflow queue, keeping RAM
    // bounded by max_assigned_batch_range without converting a normal backlog into a deadlock.
    #[allow(clippy::too_many_arguments)]
    async fn rehydrate_snark_queue(
        input: &mut PeekableReceiver<SignedBatchEnvelope<FriProof>>,
        cursor: &mut OrderedSnarkInputCursor,
        output: &mpsc::Sender<L1SenderCommand<ProofCommand>>,
        state_reporter: &ComponentStateReporter,
        proof_storage: &ProofStorage,
        committed_batch_provider: &CommittedBatchProvider,
        snark_job_manager: &SnarkJobManager,
        last_proved_batch_number: u64,
        last_committed_batch_number: u64,
        chain_id: u64,
        chain_address: Address,
        proof_mode: SnarkFriProofMode,
        journal_covered_ranges: &[(u64, u64)],
    ) -> anyhow::Result<()> {
        let recovery_from = last_proved_batch_number
            .checked_add(1)
            .context("SNARK recovery frontier overflow")?;
        let mut rehydrated_jobs = 0u64;
        for batch_number in recovery_from..=last_committed_batch_number {
            if journal_covered_ranges
                .iter()
                .any(|(from, to)| (*from..=*to).contains(&batch_number))
            {
                continue;
            }

            let stored = match proof_storage
                .get_batch_with_proof_and_age(batch_number)
                .await
            {
                Ok(Some((batch, accepted_age))) => {
                    match Self::validate_canonical_snark_fri(
                        committed_batch_provider,
                        batch_number,
                        chain_id,
                        chain_address,
                        ExpectedFriKind::Proved(proof_mode),
                        &batch,
                    )
                    .await
                    {
                        Ok(()) => Some((batch, accepted_age)),
                        Err(error) => {
                            tracing::warn!(
                                batch_number,
                                error = %format!("{error:#}"),
                                "stored FRI failed canonical recovery validation; awaiting exact recreated batch"
                            );
                            None
                        }
                    }
                }
                Ok(None) => None,
                Err(error) => {
                    tracing::warn!(
                        batch_number,
                        ?error,
                        "stored FRI could not be loaded; awaiting exact recreated batch"
                    );
                    None
                }
            };

            let (batch, accepted_age) = if let Some(stored) = stored {
                stored
            } else {
                (
                    Self::recv_recreated_fri_for(
                        input,
                        cursor,
                        batch_number,
                        last_proved_batch_number,
                        committed_batch_provider,
                        chain_id,
                        chain_address,
                        proof_mode,
                        output,
                        state_reporter,
                    )
                    .await?,
                    Duration::ZERO,
                )
            };

            tokio::select! {
                _admission = snark_job_manager.add_rehydrated_job_blocking(batch, accepted_age) => {}
                () = wait_for_snark_input_close(input) => {
                    anyhow::bail!(
                        "SNARK pipeline inbound channel closed while waiting for startup map capacity"
                    );
                }
            }
            rehydrated_jobs = rehydrated_jobs
                .checked_add(1)
                .context("SNARK recovery job count overflow")?;
        }
        tracing::info!(
            rehydrated_jobs,
            from = recovery_from,
            to = last_committed_batch_number,
            "ordered SNARK queue recovery completed"
        );
        Ok(())
    }
}

#[async_trait]
impl PipelineComponent for SnarkProvingPipelineStep {
    type Input = SignedBatchEnvelope<FriProof>;
    type Output = L1SenderCommand<ProofCommand>;

    const COMPONENT_ID: zksync_os_pipeline::ComponentId =
        zksync_os_pipeline::ComponentId::SnarkJobManager;
    const OUTPUT_CHANNEL_CAPACITY: usize = 5;

    async fn run(
        self,
        mut input: PeekableReceiver<Self::Input>,
        output: mpsc::Sender<Self::Output>,
        state_reporter: ComponentStateReporter,
    ) -> anyhow::Result<()> {
        let last_executed_batch_number = self.last_executed_batch_number;
        let last_proved_batch_number = self.last_proved_batch_number;
        let last_committed_batch_number = self.last_committed_batch_number;
        let max_fris_per_snark = self.max_fris_per_snark;
        let max_assigned_batch_range = self.max_assigned_batch_range;
        let proof_storage = self.proof_storage.clone();
        let committed_batch_provider = self.committed_batch_provider.clone();
        let snark_job_manager = self.snark_job_manager.clone();
        let preflight = self.preflight;
        let proof_mode = self.proof_mode;
        let startup_phase = self.startup_phase;
        let journal = self.journal;
        let journal_confirmations = self.journal_confirmations;
        let chain_id = self.chain_id;
        let chain_address = self.chain_address;
        let sl_provider = self.sl_provider;
        let sl_chain_id = self.sl_chain_id;
        let confirmation_policy = self.confirmation_policy;
        let mut proof_commands_receiver = self.proof_commands_receiver;
        let proof_output = output.clone();
        let proof_state_reporter = state_reporter.clone();
        let fatal_error_manager = snark_job_manager.clone();
        let mut fatal_manager_error =
            Box::pin(async move { fatal_error_manager.wait_for_fatal_error().await });

        anyhow::ensure!(
            last_executed_batch_number <= last_proved_batch_number
                && last_proved_batch_number <= last_committed_batch_number,
            "invalid SNARK startup frontiers: executed={last_executed_batch_number}, proved={last_proved_batch_number}, committed={last_committed_batch_number}"
        );
        canonical_startup_proving_version()?;
        let mut input_cursor = OrderedSnarkInputCursor::after(last_executed_batch_number)?;

        // SYSCOIN: The SNARK stage owns a production-readiness lease. A pipeline edge that is
        // already disconnected cannot authenticate recovery and must fail the critical wrapper.
        anyhow::ensure!(
            !input.is_closed(),
            "SNARK pipeline inbound channel closed before startup recovery"
        );
        anyhow::ensure!(
            !output.is_closed(),
            "SNARK pipeline outbound channel closed before startup recovery"
        );

        // SYSCOIN: Only the historical state block with the selected prove sender's inclusive
        // confirmation depth may authorize startup deletion. Skip the extra RPC snapshot when the
        // journal is empty; fake proving is required to have no real wrapper records at all.
        let mut frontiers = if journal.has_records().await {
            let snapshot = canonical_proved_frontiers(
                &sl_provider,
                chain_address,
                sl_chain_id,
                confirmation_policy,
            )
            .await?;
            anyhow::ensure!(
                snapshot.latest_proved == last_proved_batch_number,
                "startup proved frontier changed before durable SNARK journal recovery; restart full L1 discovery"
            );
            Some(snapshot)
        } else {
            None
        };
        let confirmation_safe_proved = frontiers
            .as_ref()
            .map_or(last_proved_batch_number, |snapshot| {
                snapshot.confirmation_safe_proved
            });

        // SYSCOIN: Canonical-state validate and classify every durable wrapper before exposing any
        // rehydrated FRI to aggregation. Latest-covered but not depth-safe records remain durable
        // while startup waits synchronously; the Gapless cursor cannot advance past them.
        let mut recovered = journal
            .recover(
                last_proved_batch_number,
                confirmation_safe_proved,
                last_committed_batch_number,
                chain_id,
                chain_address,
                proof_mode == SnarkFriProofMode::Real,
                &committed_batch_provider,
            )
            .await?;

        if !recovered.pending_confirmation.is_empty() {
            let pending_from = recovered
                .pending_confirmation
                .iter()
                .map(|record| record.batch_range().0)
                .min()
                .expect("pending startup confirmations are non-empty");
            let pending_to = recovered
                .pending_confirmation
                .iter()
                .map(|record| record.batch_range().1)
                .max()
                .expect("pending startup confirmations are non-empty");
            loop {
                anyhow::ensure!(
                    !input.is_closed(),
                    "SNARK pipeline inbound channel closed while awaiting startup journal confirmation depth"
                );
                anyhow::ensure!(
                    !output.is_closed(),
                    "SNARK pipeline outbound channel closed while awaiting startup journal confirmation depth"
                );
                let snapshot = frontiers.expect("journal records require a canonical snapshot");
                if snapshot.confirmation_safe_proved >= pending_to {
                    journal
                        .retire_startup_confirmed(
                            &recovered.pending_confirmation,
                            snapshot.confirmation_safe_proved,
                        )
                        .await?;
                    break;
                }
                tracing::info!(
                    pending_from,
                    pending_to,
                    latest_proved = snapshot.latest_proved,
                    confirmation_safe_proved = snapshot.confirmation_safe_proved,
                    tip_block = snapshot.tip_block,
                    safe_block = snapshot.safe_block,
                    required_confirmations = confirmation_policy.required_confirmations(),
                    "waiting for startup-covered durable SNARK journal confirmation depth"
                );
                let refreshed = await_startup_confirmation_refresh(&input, &output, async {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    canonical_proved_frontiers(
                        &sl_provider,
                        chain_address,
                        sl_chain_id,
                        confirmation_policy,
                    )
                    .await
                })
                .await?;
                ensure_startup_proved_frontier_unchanged(
                    last_proved_batch_number,
                    refreshed.latest_proved,
                )?;
                frontiers = Some(refreshed);
            }
            recovered.pending_confirmation.clear();
        }

        let journal_covered_ranges: Vec<_> = recovered
            .replay
            .iter()
            .map(|journaled| journaled.batch_range())
            .collect();
        let recovery_plan = StartupRecoveryPlan::build(
            last_proved_batch_number,
            last_committed_batch_number,
            &journal_covered_ranges,
            max_fris_per_snark,
            max_assigned_batch_range,
            proof_mode == SnarkFriProofMode::Fake,
        )
        .context("build ordered SNARK startup recovery plan")?;
        // SYSCOIN: Canonically validated journal ranges acquire in-process ownership before any
        // queue or prover can observe the startup map. Reaped files retain these tombstones until
        // process exit, preventing recreated FRI duplicates from reopening completed work.
        snark_job_manager
            .seed_recovered_journal_ownership(&journal_covered_ranges)
            .await
            .context("seed recovered durable SNARK ownership")?;
        snark_job_manager
            .install_startup_recovery_plan(recovery_plan)
            .await
            .context("install ordered SNARK startup recovery boundary")?;
        let confirmation_sender = journal.confirmation_sender();
        for journaled in recovered.replay {
            // SYSCOIN: A durable file is crash authority, not proof validity authority. Re-run the
            // exact fixed-block verifier preflight before recovered bytes can re-enter L1 sending.
            let (batches, proof) = journaled.preflight_inputs();
            preflight.verify(batches, proof).await.map_err(|error| {
                anyhow::anyhow!("recovered SNARK verifier preflight failed: {error}")
            })?;
            proof_output
                .send_and_record(
                    L1SenderCommand::SendToL1(journaled.into_command(confirmation_sender.clone())),
                    &proof_state_reporter,
                )
                .await?;
        }

        // SYSCOIN: Keep completed SNARK proofs and confirmation cleanup draining while rehydration
        // may wait for queue space. A closed reaper is node-critical; confirmed files remain safe.
        let mut proof_forwarder = tokio::spawn(async move {
            while let Some(proof_command) = proof_commands_receiver.recv().await {
                proof_output
                    .send_and_record(
                        L1SenderCommand::SendToL1(proof_command),
                        &proof_state_reporter,
                    )
                    .await?;
            }
            Ok::<(), anyhow::Error>(())
        });
        let mut journal_reaper = tokio::spawn(journal.run_reaper(journal_confirmations));
        let _proof_forwarder_abort = AbortTaskOnDrop(proof_forwarder.abort_handle());
        let _journal_reaper_abort = AbortTaskOnDrop(journal_reaper.abort_handle());

        // SYSCOIN: Journal authority is now classified and replayed, and both supervised drain
        // tasks exist. Re-check the pipeline edges immediately before exposing the prover listener.
        if input.is_closed() {
            proof_forwarder.abort();
            journal_reaper.abort();
            anyhow::bail!("SNARK pipeline inbound channel closed before prover draining");
        }
        if output.is_closed() {
            proof_forwarder.abort();
            journal_reaper.abort();
            anyhow::bail!("SNARK pipeline outbound channel closed before prover draining");
        }

        // SYSCOIN: Drainable opens only the authenticated prover surface. Public node readiness
        // remains closed while ordered disk overflow blocks on workers draining the bounded map.
        advance_startup_phase(&startup_phase, SnarkStartupPhase::Drainable);

        tokio::select! {
            result = Self::rehydrate_snark_queue(
                &mut input,
                &mut input_cursor,
                &output,
                &state_reporter,
                &proof_storage,
                &committed_batch_provider,
                &snark_job_manager,
                last_proved_batch_number,
                last_committed_batch_number,
                chain_id,
                chain_address,
                proof_mode,
                &journal_covered_ranges,
            ) => {
                result?;
            }
            result = &mut proof_forwarder => {
                result??;
                journal_reaper.abort();
                anyhow::bail!("SNARK proof-command channel closed during startup recovery");
            }
            result = &mut journal_reaper => {
                proof_forwarder.abort();
                result??;
                anyhow::bail!("durable SNARK journal reaper stopped");
            }
            message = &mut fatal_manager_error => {
                proof_forwarder.abort();
                journal_reaper.abort();
                anyhow::bail!("terminal SNARK manager fault: {message}");
            }
            _ = output.closed() => {
                proof_forwarder.abort();
                journal_reaper.abort();
                anyhow::bail!("SNARK pipeline outbound channel closed during startup recovery");
            }
        }

        // SYSCOIN: Re-check both pipeline edges after potentially long recovery. An inbound close
        // that raced rehydration must not leave a retained Ready phase for readiness consumers.
        if input.is_closed() {
            proof_forwarder.abort();
            journal_reaper.abort();
            anyhow::bail!("SNARK pipeline inbound channel closed during startup recovery");
        }
        if output.is_closed() {
            proof_forwarder.abort();
            journal_reaper.abort();
            anyhow::bail!("SNARK pipeline outbound channel closed during startup recovery");
        }

        // SYSCOIN: Ready is intentionally after complete stored-FRI rehydration. The prover
        // listener may already drain at Drainable, but no public node-ready latch may precede this.
        snark_job_manager
            .finish_startup_loading()
            .await
            .context("finish ordered SNARK startup loading")?;
        advance_startup_phase(&startup_phase, SnarkStartupPhase::Ready);

        // SYSCOIN: Every sibling lifecycle after readiness is fail-critical; no closed pipeline,
        // proof channel, journal reaper, or manager may leave a stale live readiness lease.
        tokio::select! {
            result = async {
                while let Some(batch) = input_cursor
                    .recv_next(&mut input, &state_reporter)
                    .await?
                {
                    let batch_number = batch.batch_number();
                    if batch_number <= last_proved_batch_number {
                        Self::validate_canonical_snark_fri(
                            &committed_batch_provider,
                            batch_number,
                            chain_id,
                            chain_address,
                            ExpectedFriKind::Passthrough,
                            &batch,
                        )
                        .await?;
                        let passthrough = L1SenderCommand::Passthrough(Box::new(batch));
                        output.send_and_record(passthrough, &state_reporter).await?;
                        continue;
                    }

                    Self::validate_canonical_snark_fri(
                        &committed_batch_provider,
                        batch_number,
                        chain_id,
                        chain_address,
                        ExpectedFriKind::Proved(proof_mode),
                        &batch,
                    )
                    .await?;
                    if batch_number <= last_committed_batch_number {
                        // SYSCOIN: Startup already classified every committed FRI from durable
                        // disk, journal ownership, or an exact recreated fallback. Validate its
                        // live copy above, then discard it without perturbing a lease or wait age.
                        tracing::info!(
                            batch_number,
                            "discarding canonically validated startup FRI duplicate"
                        );
                        continue;
                    }
                    let _ = snark_job_manager.add_job(batch).await;
                }
                Ok::<(), anyhow::Error>(())
            } => {
                proof_forwarder.abort();
                journal_reaper.abort();
                result?;
                anyhow::bail!("SNARK pipeline inbound channel closed after startup recovery");
            },
            result = &mut proof_forwarder => {
                journal_reaper.abort();
                result??;
                anyhow::bail!("SNARK proof-command channel closed after startup recovery");
            },
            result = &mut journal_reaper => {
                proof_forwarder.abort();
                result??;
                anyhow::bail!("durable SNARK journal reaper stopped");
            },
            message = &mut fatal_manager_error => {
                proof_forwarder.abort();
                journal_reaper.abort();
                anyhow::bail!("terminal SNARK manager fault: {message}");
            },
            _ = output.closed() => {
                proof_forwarder.abort();
                journal_reaper.abort();
                anyhow::bail!("SNARK pipeline outbound channel closed after startup recovery");
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProofStorageConfig;
    use crate::prover_api::proof_storage::StoredBatch;
    use crate::prover_api::snark_proof_journal::reconstruct_execute_root;
    use crate::prover_api::test_util::create_test_batch_envelope_with_data;
    use alloy::network::EthereumWallet;
    use alloy::primitives::{Bytes, U64, U256};
    use alloy::providers::ProviderBuilder;
    use alloy::rpc::json_rpc::ErrorPayload;
    use alloy::rpc::types::Block;
    use alloy::sol_types::SolValue;
    use alloy::transports::mock::Asserter;
    use reth_tasks::Runtime;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use tempfile::TempDir;
    use zksync_os_batch_types::DiscoveredCommittedBatch;
    use zksync_os_batch_types::batcher_model::{
        BatchForSigning, BatchMetadata, BatchSignatureData, RealFriProof,
    };
    use zksync_os_contract_interface::l1_discovery::{BatchVerificationSL, L1State};
    use zksync_os_contract_interface::models::BatchDaInputMode;
    use zksync_os_contract_interface::settlement_layer_intervals::SettlementLayerIntervals;
    use zksync_os_contract_interface::{Bridgehub, ZkChain};
    use zksync_os_storage_api::{PersistedBatch, ReadBatch};
    use zksync_os_types::{ProtocolSemanticVersion, ProvingVersion};

    const SL_CHAIN_ID: u64 = 270;
    const DIAMOND: Address = Address::new([0x11; 20]);

    fn block(number: u64, hash: B256) -> Block {
        let mut block: Block = Block::default();
        block.header.inner.number = number;
        block.header.hash = hash;
        block
    }

    async fn mocked_provider(asserter: &Asserter) -> NodeProvider {
        let capability = block(1, B256::new([1; 32])).header;
        asserter.push_success(&capability);
        asserter.push_success(&capability);
        asserter.push_failure(ErrorPayload::method_not_found());
        asserter.push_success(&"anvil/v1.0.0".to_owned());
        let provider = ProviderBuilder::new()
            .disable_recommended_fillers()
            .wallet(EthereumWallet::default())
            .connect_mocked_client(asserter.clone());
        NodeProvider::new(provider)
            .await
            .expect("mocked provider construction")
    }

    #[derive(Clone, Default)]
    struct RecoveryBatchStorage {
        batches: Arc<Mutex<HashMap<u64, PersistedBatch>>>,
    }

    impl RecoveryBatchStorage {
        fn insert(&self, batch: PersistedBatch) {
            self.batches
                .lock()
                .expect("recovery batch storage lock poisoned")
                .insert(batch.number(), batch);
        }
    }

    impl ReadBatch for RecoveryBatchStorage {
        fn get_batch_by_block_number(
            &self,
            block_number: u64,
        ) -> anyhow::Result<Option<PersistedBatch>> {
            Ok(self
                .batches
                .lock()
                .expect("recovery batch storage lock poisoned")
                .values()
                .find(|batch| batch.block_range.contains(&block_number))
                .cloned())
        }

        fn get_batch_by_number(&self, batch_number: u64) -> anyhow::Result<Option<PersistedBatch>> {
            Ok(self
                .batches
                .lock()
                .expect("recovery batch storage lock poisoned")
                .get(&batch_number)
                .cloned())
        }

        fn latest_batch(&self) -> u64 {
            self.batches
                .lock()
                .expect("recovery batch storage lock poisoned")
                .keys()
                .max()
                .copied()
                .unwrap_or_default()
        }
    }

    struct RecoveryFixture {
        _directory: TempDir,
        _runtime: Runtime,
        asserter: Asserter,
        step: SnarkProvingPipelineStep,
        manager: Arc<SnarkJobManager>,
        readiness: watch::Receiver<SnarkStartupPhase>,
        canonical_batches: HashMap<u64, BatchMetadata>,
    }

    // SYSCOIN: Build restart state entirely from persisted canonical batches, keeping the mock
    // provider queue empty so capacity is the only possible recovery failure in focused tests.
    async fn recovery_fixture(
        proof_batches: &[u64],
        last_committed_batch: u64,
        max_assigned_batch_range: usize,
    ) -> anyhow::Result<RecoveryFixture> {
        let directory = TempDir::new()?;
        let proof_storage_path = directory.path().to_owned();
        let proof_storage = ProofStorage::new(ProofStorageConfig {
            path: proof_storage_path.clone(),
            ..ProofStorageConfig::default()
        })
        .await?;
        let storage = RecoveryBatchStorage::default();
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let mut genesis_batch_info = None;
        let mut previous_stored_batch_info = None;
        let mut canonical_batches = HashMap::new();

        for batch_number in 1..=last_committed_batch {
            let mut batch = create_test_batch_envelope_with_data(
                batch_number,
                protocol_version.clone(),
                FriProof::Fake,
            );
            batch.batch.chain_address = DIAMOND;
            if let Some(previous) = previous_stored_batch_info {
                batch.batch.previous_stored_batch_info = previous;
            }
            // SYSCOIN: Recovery fixtures carry the exact V32 execute opening now enforced for
            // production disk and recreated FRIs, including the canonical predecessor chain.
            batch.batch.batch_info.commit_info.l2_to_l1_logs_root_hash =
                reconstruct_execute_root(&batch.batch)?;
            let canonical_metadata = batch.batch.clone();
            let committed_batch = DiscoveredCommittedBatch {
                batch_info: batch.batch.batch_info.clone().into_stored(),
                block_range: batch_number..=batch_number,
            };
            previous_stored_batch_info = Some(committed_batch.batch_info.clone());
            genesis_batch_info.get_or_insert_with(|| committed_batch.batch_info.clone());
            storage.insert(PersistedBatch {
                committed_batch,
                execute_sl_block_number: Some(batch_number),
            });
            if proof_batches.contains(&batch_number) {
                proof_storage
                    .save_batch_with_proof(&StoredBatch(batch))
                    .await?;
            }
            canonical_batches.insert(batch_number, canonical_metadata);
        }

        let asserter = Asserter::new();
        let sl_provider = mocked_provider(&asserter).await;
        let diamond_proxy = ZkChain::new(DIAMOND, sl_provider.clone());
        let bridgehub = Bridgehub::new(Address::new([0x22; 20]), sl_provider.clone(), 1);
        let l1_state = L1State {
            bridgehub_l1: bridgehub.clone(),
            bridgehub_sl: bridgehub,
            diamond_proxy_l1: diamond_proxy.clone(),
            diamond_proxy_sl: diamond_proxy.clone(),
            validator_timelock_sl: Address::new([0x33; 20]),
            batch_verification: BatchVerificationSL::Disabled,
            last_committed_batch,
            last_proved_batch: 1,
            last_executed_batch: 1,
            last_finalized_executed_batch: 1,
            sl_block_number: 0,
            finalized_sl_block_number: 0,
            da_input_mode: BatchDaInputMode::Rollup,
            l1_chain_id: 1,
            sl_chain_id: SL_CHAIN_ID,
            settlement_layer_address: Address::ZERO,
            settlement_layer_intervals: SettlementLayerIntervals::direct_l1(diamond_proxy),
        };
        let runtime = Runtime::test();
        let genesis_batch_info = genesis_batch_info.context("missing recovery genesis batch")?;
        let committed_batch_provider = CommittedBatchProvider::new(
            &runtime,
            &l1_state,
            16,
            storage,
            None,
            move || async move { genesis_batch_info },
        )
        .await?;
        for batch_number in 1..=last_committed_batch {
            tokio::time::timeout(
                Duration::from_secs(2),
                committed_batch_provider.wait_for_batch(batch_number),
            )
            .await
            .with_context(|| format!("load persisted committed batch {batch_number}"))?;
        }

        let (journal, journal_confirmations) = SnarkProofJournal::open(&proof_storage_path).await?;
        let (step, manager, readiness) = SnarkProvingPipelineStep::new(
            proof_storage,
            2,
            2,
            Duration::from_secs(60),
            1,
            1,
            last_committed_batch,
            Duration::from_secs(60),
            max_assigned_batch_range,
            committed_batch_provider,
            journal,
            journal_confirmations,
            1,
            DIAMOND,
            sl_provider,
            SL_CHAIN_ID,
            1,
            B256::ZERO,
            true,
        );

        Ok(RecoveryFixture {
            _directory: directory,
            _runtime: runtime,
            asserter,
            step,
            manager,
            readiness,
            canonical_batches,
        })
    }

    // SYSCOIN: Recovery-input tests reconstruct a fresh signed envelope from the authoritative
    // metadata snapshot because proof-bearing envelopes intentionally are not Clone.
    fn recreated_fake_batch(
        canonical_batches: &HashMap<u64, BatchMetadata>,
        batch_number: u64,
    ) -> SignedBatchEnvelope<FriProof> {
        BatchForSigning::new(canonical_batches[&batch_number].clone(), FriProof::Fake)
            .with_signatures(BatchSignatureData::NotNeeded)
    }

    fn real_proof() -> FriProof {
        FriProof::Real(RealFriProof {
            proof: Bytes::from_static(b"proof"),
            proving_execution_version: ProvingVersion::V8 as u32,
        })
    }

    #[test]
    fn proof_mode_never_accepts_cross_mode_or_l1_marker() {
        let real = real_proof();
        assert!(SnarkFriProofMode::Real.accepts(&real));
        assert!(!SnarkFriProofMode::Fake.accepts(&real));
        assert!(SnarkFriProofMode::Fake.accepts(&FriProof::Fake));
        assert!(!SnarkFriProofMode::Real.accepts(&FriProof::Fake));

        // SYSCOIN: The passthrough marker carries no FRI bytes and must never become a real or
        // fake SNARK job after restart, even if durable state outlives a testnet transition.
        assert!(!SnarkFriProofMode::Real.accepts(&FriProof::AlreadySubmittedToL1));
        assert!(!SnarkFriProofMode::Fake.accepts(&FriProof::AlreadySubmittedToL1));
    }

    // SYSCOIN: The stage-owned publisher cannot skip Drainable, repeat a phase, or regress after
    // public readiness; watch coalescing therefore cannot conceal an invalid production transition.
    #[test]
    fn startup_phase_publisher_allows_only_the_monotonic_sequence() {
        let (sender, receiver) = watch::channel(SnarkStartupPhase::Recovering);
        advance_startup_phase(&sender, SnarkStartupPhase::Drainable);
        assert_eq!(*receiver.borrow(), SnarkStartupPhase::Drainable);
        advance_startup_phase(&sender, SnarkStartupPhase::Ready);
        assert_eq!(*receiver.borrow(), SnarkStartupPhase::Ready);
        assert!(
            !SnarkStartupPhase::Recovering.can_advance_to(SnarkStartupPhase::Ready),
            "Recovering may not skip the Drainable barrier"
        );
        assert!(
            !SnarkStartupPhase::Ready.can_advance_to(SnarkStartupPhase::Drainable),
            "Ready may not regress to Drainable"
        );
    }

    #[test]
    fn pending_confirmation_wait_restarts_on_any_proved_frontier_change() {
        // SYSCOIN: Startup classified pending 1-2 and replay 3-4 against latest=2. A concurrent
        // advance to four must restart before either stale classification can retire or replay.
        ensure_startup_proved_frontier_unchanged(2, 4).unwrap_err();
        ensure_startup_proved_frontier_unchanged(2, 1).unwrap_err();
        ensure_startup_proved_frontier_unchanged(2, 2).unwrap();
    }

    // SYSCOIN: A pending confirmation refresh includes its delay and RPC work, but neither may
    // retain Recovering forever after the recreated upstream or L1-sender downstream disappears.
    #[tokio::test]
    async fn pending_confirmation_refresh_observes_both_pipeline_edges() {
        let (input_sender, input_receiver) = mpsc::channel::<SignedBatchEnvelope<FriProof>>(1);
        let input = PeekableReceiver::new(input_receiver);
        let (output_sender, output_receiver) = mpsc::channel::<L1SenderCommand<ProofCommand>>(1);
        drop(input_sender);

        let inbound_error = tokio::time::timeout(
            Duration::from_secs(1),
            await_startup_confirmation_refresh(
                &input,
                &output_sender,
                std::future::pending::<anyhow::Result<()>>(),
            ),
        )
        .await
        .expect("closed startup input was not observed")
        .expect_err("closed startup input must stop confirmation refresh");
        assert!(inbound_error.to_string().contains("inbound channel closed"));
        drop(output_receiver);

        let (_input_sender, input_receiver) = mpsc::channel::<SignedBatchEnvelope<FriProof>>(1);
        let input = PeekableReceiver::new(input_receiver);
        let (output_sender, output_receiver) = mpsc::channel::<L1SenderCommand<ProofCommand>>(1);
        drop(output_receiver);

        let outbound_error = tokio::time::timeout(
            Duration::from_secs(1),
            await_startup_confirmation_refresh(
                &input,
                &output_sender,
                std::future::pending::<anyhow::Result<()>>(),
            ),
        )
        .await
        .expect("closed startup output was not observed")
        .expect_err("closed startup output must stop confirmation refresh");
        assert!(
            outbound_error
                .to_string()
                .contains("outbound channel closed")
        );
    }

    // SYSCOIN: Three inclusive confirmations make tip-2 the exact historical state frontier;
    // both counters are hash-anchored and every dependency is consumed from one mock snapshot.
    #[tokio::test]
    async fn proved_frontiers_use_confirmation_safe_canonical_hash() {
        let asserter = Asserter::new();
        let provider = mocked_provider(&asserter).await;
        let tip = block(42, B256::new([0x42; 32]));
        let safe = block(40, B256::new([0x40; 32]));
        asserter.push_success(&U64::from(SL_CHAIN_ID));
        asserter.push_success(&tip);
        asserter.push_success(&safe);
        asserter.push_success(&Bytes::from(U256::from(10).abi_encode()));
        asserter.push_success(&Bytes::from(U256::from(8).abi_encode()));
        asserter.push_success(&tip);
        asserter.push_success(&safe);
        asserter.push_success(&U64::from(SL_CHAIN_ID));

        let snapshot =
            canonical_proved_frontiers(&provider, DIAMOND, SL_CHAIN_ID, ConfirmationPolicy::new(3))
                .await
                .unwrap();
        assert_eq!(
            snapshot,
            CanonicalProvedFrontiers {
                tip_block: 42,
                safe_block: 40,
                latest_proved: 10,
                confirmation_safe_proved: 8,
            }
        );
        assert!(asserter.read_q().is_empty(), "all RPC responses consumed");
    }

    // SYSCOIN: Gateway's one-confirmation policy uses latest itself, while a numbered postcheck
    // still converts a concurrent replacement into a startup restart rather than journal deletion.
    #[tokio::test]
    async fn one_confirmation_uses_tip_and_rejects_reorg() {
        let asserter = Asserter::new();
        let provider = mocked_provider(&asserter).await;
        let tip = block(42, B256::new([0x42; 32]));
        let replacement = block(42, B256::new([0x43; 32]));
        asserter.push_success(&U64::from(SL_CHAIN_ID));
        asserter.push_success(&tip);
        asserter.push_success(&Bytes::from(U256::from(10).abi_encode()));
        asserter.push_success(&Bytes::from(U256::from(10).abi_encode()));
        asserter.push_success(&replacement);

        let error =
            canonical_proved_frontiers(&provider, DIAMOND, SL_CHAIN_ID, ConfirmationPolicy::new(1))
                .await
                .unwrap_err();
        assert!(error.to_string().contains("reorged"));
        assert!(asserter.read_q().is_empty(), "all RPC responses consumed");
    }

    // SYSCOIN: A restart backlog larger than the RAM span reaches Drainable, waits on bounded
    // admission, and resumes only after a worker completes the exact planned head. Public Ready
    // remains closed throughout the wait and no out-of-order batch enters the map.
    #[tokio::test]
    async fn recovered_fri_backlog_drains_boundedly_before_readiness() -> anyhow::Result<()> {
        let RecoveryFixture {
            _directory,
            _runtime,
            asserter: _asserter,
            step,
            manager,
            mut readiness,
            canonical_batches: _canonical_batches,
        } = recovery_fixture(&[2, 3, 4], 4, 1).await?;
        let (input_sender, input_receiver) = mpsc::channel(1);
        let (output_sender, output_receiver) = mpsc::channel(1);
        let (state_reporter, _state_receiver) =
            ComponentStateReporter::new("snark_recovery_bounded_drain_test");
        let run = tokio::spawn(step.run(
            PeekableReceiver::new(input_receiver),
            output_sender,
            state_reporter,
        ));

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let phase = *readiness.borrow_and_update();
                if phase == SnarkStartupPhase::Drainable {
                    break Ok::<(), watch::error::RecvError>(());
                }
                readiness.changed().await?;
            }
        })
        .await
        .context("oversized recovery did not publish Drainable")??;

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let queued: Vec<_> = manager
                    .status()
                    .await
                    .into_iter()
                    .map(|state| state.fri_job.batch_number)
                    .collect();
                if queued == [2, 3] {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .context("recovery did not fill the exact bounded head")?;
        assert_eq!(
            *readiness.borrow_and_update(),
            SnarkStartupPhase::Drainable,
            "bounded recovery published Ready before capacity was released"
        );

        // `pick_real_job` first consumes fake-lane startup work through the same manager path used
        // by FakeSnarkProver. Completing 2-3 releases map space for disk batch 4.
        let _ = manager
            .pick_real_job("startup-drainer".to_owned(), None)
            .await
            .context("drain fake startup head")?;

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if *readiness.borrow_and_update() == SnarkStartupPhase::Ready {
                    break Ok::<(), watch::error::RecvError>(());
                }
                readiness.changed().await?;
            }
        })
        .await
        .context("bounded recovery did not publish Ready after worker drain")??;
        let queued: Vec<_> = manager
            .status()
            .await
            .into_iter()
            .map(|state| state.fri_job.batch_number)
            .collect();
        assert_eq!(
            queued,
            vec![4],
            "recovery did not retain only the post-drain startup tail"
        );

        drop(input_sender);
        let run_error = tokio::time::timeout(Duration::from_secs(2), run)
            .await
            .context("SNARK pipeline did not observe its closed test input")??
            .expect_err("closing the input after readiness must stop the pipeline");
        assert!(
            run_error
                .to_string()
                .contains("inbound channel closed after startup recovery")
        );
        drop(output_receiver);
        Ok(())
    }

    // SYSCOIN: Capacity backpressure does not consume input. Sender liveness is polled separately
    // so a dead upstream closes the Drainable lease instead of wedging startup behind a full map.
    #[tokio::test]
    async fn capacity_wait_fails_when_recreated_pipeline_closes() -> anyhow::Result<()> {
        let RecoveryFixture {
            _directory,
            _runtime,
            asserter: _asserter,
            step,
            manager,
            mut readiness,
            canonical_batches: _canonical_batches,
        } = recovery_fixture(&[2, 3, 4], 4, 1).await?;
        let (input_sender, input_receiver) = mpsc::channel(1);
        let (output_sender, output_receiver) = mpsc::channel(1);
        let (state_reporter, _state_receiver) =
            ComponentStateReporter::new("snark_recovery_capacity_input_close_test");
        let run = tokio::spawn(step.run(
            PeekableReceiver::new(input_receiver),
            output_sender,
            state_reporter,
        ));

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if *readiness.borrow_and_update() == SnarkStartupPhase::Drainable {
                    break Ok::<(), watch::error::RecvError>(());
                }
                readiness.changed().await?;
            }
        })
        .await
        .context("capacity-close fixture did not publish Drainable")??;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let queued: Vec<_> = manager
                    .status()
                    .await
                    .into_iter()
                    .map(|state| state.fri_job.batch_number)
                    .collect();
                if queued == [2, 3] {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .context("capacity-close fixture did not reach its bounded map wait")?;

        drop(input_sender);
        let run_error = tokio::time::timeout(Duration::from_secs(2), run)
            .await
            .context("capacity wait ignored closed recreated pipeline")??
            .expect_err("closed recreated pipeline must terminate startup recovery");
        assert!(
            run_error
                .to_string()
                .contains("inbound channel closed while waiting for startup map capacity"),
            "unexpected capacity-close error: {run_error:#}"
        );
        assert_eq!(*readiness.borrow_and_update(), SnarkStartupPhase::Drainable);
        assert!(
            readiness.changed().await.is_err(),
            "failed startup retained a live Drainable sender"
        );
        drop(output_receiver);
        Ok(())
    }

    // SYSCOIN: Recovery treats disk as an ordered overflow queue. A later stored FRI may not enter
    // RAM while an earlier artifact is missing; the exact recreated duplicate stream must supply
    // and validate the gap first.
    #[tokio::test]
    async fn sparse_disk_recovery_waits_for_exact_recreated_gap() -> anyhow::Result<()> {
        let RecoveryFixture {
            _directory,
            _runtime,
            asserter: _asserter,
            step,
            manager,
            mut readiness,
            canonical_batches,
        } = recovery_fixture(&[2, 4], 4, 8).await?;
        let (input_sender, input_receiver) = mpsc::channel(1);
        let (output_sender, output_receiver) = mpsc::channel(1);
        let (state_reporter, _state_receiver) =
            ComponentStateReporter::new("snark_sparse_disk_recovery_test");
        let run = tokio::spawn(step.run(
            PeekableReceiver::new(input_receiver),
            output_sender,
            state_reporter,
        ));

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if *readiness.borrow_and_update() == SnarkStartupPhase::Drainable {
                    break Ok::<(), watch::error::RecvError>(());
                }
                readiness.changed().await?;
            }
        })
        .await
        .context("sparse recovery did not publish Drainable")??;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let queued: Vec<_> = manager
                    .status()
                    .await
                    .into_iter()
                    .map(|state| state.fri_job.batch_number)
                    .collect();
                if queued == [2] {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .context("stored batch 2 was not admitted before the missing gap")?;

        input_sender
            .send(recreated_fake_batch(&canonical_batches, 2))
            .await
            .context("send recreated startup duplicate 2")?;
        // Waiting for a fresh permit proves the cursor consumed batch 2 and is now blocked on 3.
        drop(
            input_sender
                .reserve()
                .await
                .context("wait for recreated batch 2 consumption")?,
        );
        let queued_before_gap: Vec<_> = manager
            .status()
            .await
            .into_iter()
            .map(|state| state.fri_job.batch_number)
            .collect();
        assert_eq!(
            queued_before_gap,
            vec![2],
            "stored batch 4 jumped the missing batch 3 recovery gap"
        );

        input_sender
            .send(recreated_fake_batch(&canonical_batches, 3))
            .await
            .context("send recreated missing batch 3")?;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if *readiness.borrow_and_update() == SnarkStartupPhase::Ready {
                    break Ok::<(), watch::error::RecvError>(());
                }
                readiness.changed().await?;
            }
        })
        .await
        .context("sparse recovery did not reach Ready after exact gap replay")??;
        let queued: Vec<_> = manager
            .status()
            .await
            .into_iter()
            .map(|state| state.fri_job.batch_number)
            .collect();
        assert_eq!(queued, vec![2, 3, 4]);

        drop(input_sender);
        let run_error = tokio::time::timeout(Duration::from_secs(2), run)
            .await
            .context("SNARK pipeline did not observe its closed sparse test input")??
            .expect_err("closing sparse recovery input after readiness must stop the pipeline");
        assert!(
            run_error
                .to_string()
                .contains("inbound channel closed after startup recovery")
        );
        drop(output_receiver);
        Ok(())
    }

    // SYSCOIN: Equality is the configured maximum difference, so a contiguous exact-bound restart
    // advances through Drainable and publishes Ready after complete durable rehydration.
    #[tokio::test]
    async fn recovered_fri_span_at_exact_bound_publishes_readiness() -> anyhow::Result<()> {
        let RecoveryFixture {
            _directory,
            _runtime,
            asserter: _asserter,
            step,
            manager,
            mut readiness,
            canonical_batches: _canonical_batches,
        } = recovery_fixture(&[2, 3], 3, 1).await?;
        let (input_sender, input_receiver) = mpsc::channel(1);
        let (output_sender, output_receiver) = mpsc::channel(1);
        let (state_reporter, _state_receiver) =
            ComponentStateReporter::new("snark_recovery_exact_capacity_test");
        let run = tokio::spawn(step.run(
            PeekableReceiver::new(input_receiver),
            output_sender,
            state_reporter,
        ));

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if *readiness.borrow_and_update() == SnarkStartupPhase::Ready {
                    break Ok::<(), watch::error::RecvError>(());
                }
                readiness.changed().await?;
            }
        })
        .await
        .context("exact-bound recovery did not publish Ready")??;
        let queued: Vec<_> = manager
            .status()
            .await
            .into_iter()
            .map(|state| state.fri_job.batch_number)
            .collect();
        assert_eq!(queued, vec![2, 3]);
        drop(input_sender);
        let run_error = tokio::time::timeout(Duration::from_secs(2), run)
            .await
            .context("SNARK pipeline did not observe its closed test input")??
            .expect_err("closing the input after readiness must stop the pipeline");
        assert!(
            run_error
                .to_string()
                .contains("inbound channel closed after startup recovery")
        );
        drop(output_receiver);
        Ok(())
    }
}
