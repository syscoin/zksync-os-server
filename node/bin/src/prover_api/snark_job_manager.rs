use crate::prover_api::fri_job_manager::FriJob;
use crate::prover_api::fri_job_manager::JobState;
use crate::prover_api::metrics::{ProverStage, ProverType};
use crate::prover_api::prover_job_map::{
    BeginSubmissionError, JobEntry, JobMapCapacityExceeded, ProverJobMap, SnarkCompletedOwner,
    SnarkJobAdmission, SnarkJobEligibility, SnarkJobPick, SnarkOwnershipCompletion,
    SnarkOwnershipSeedError, StartupRecoveryBoundaryError, StartupRecoveryPlan,
};
use crate::prover_api::snark_proof_journal::SnarkProofJournal;
use crate::prover_api::snark_proof_preflight::{SnarkProofPreflight, SnarkProofPreflightError};
use base64::{Engine as _, engine::general_purpose};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
#[cfg(test)]
use tokio::sync::Semaphore;
use tokio::sync::mpsc;
use tokio::sync::mpsc::Permit;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::watch;
#[cfg(test)]
use zksync_os_batch_types::batcher_model::BatchMetadata;
use zksync_os_batch_types::batcher_model::{
    BatchEnvelope, BatchSignatureData, FriProof, RealSnarkProof, SignedBatchEnvelope, SnarkProof,
};
use zksync_os_batcher_metrics::BatchExecutionStage;
use zksync_os_l1_sender::commands::prove::{ProofCommand, ZKSYNC_OS_V8_REAL_PROOF_BYTES};
use zksync_os_types::ProvingVersion;

// SYSCOIN: Bound the normal compressed/uncompressed SNARK-pick JSON below a small fraction of a
// 64-GiB host. The fixed budget covers field names, VK/token/range values, quotes, and commas.
pub(crate) const MAX_SNARK_PICK_RESPONSE_BYTES: usize = 256 * 1024 * 1024;
const SNARK_PICK_FIXED_JSON_BUDGET: usize = 4 * 1024;

// SYSCOIN: Base64 expands to four bytes per three-byte quantum. Include JSON quotes and one comma
// per element with checked arithmetic before a range receives an opaque lease.
fn snark_pick_proof_wire_bytes(proof: &FriProof) -> Option<usize> {
    let FriProof::Real(real) = proof else {
        return None;
    };
    real.proof()
        .len()
        .checked_add(2)?
        .checked_div(3)?
        .checked_mul(4)?
        .checked_add(3)
}

/// SYSCOIN: Preserve retry semantics across the HTTP boundary without string-matching anyhow
/// messages; only capacity failures retain the exact lease for automatic proof retry.
#[derive(Debug, Error)]
pub enum SnarkSubmitError {
    #[error("invalid batch range: from batch {from} is greater than to batch {to}")]
    InvalidRange { from: u64, to: u64 },
    #[error("SNARK proof payload must not be empty")]
    EmptyProof,
    #[error(
        "V8 SNARK proof payload must be exactly {ZKSYNC_OS_V8_REAL_PROOF_BYTES} bytes; got {0}"
    )]
    InvalidProofLength(usize),
    #[error("invalid SNARK proof base64: {0}")]
    InvalidBase64(String),
    #[error("no Proving Version matches the provided verification key: {0}")]
    UnknownVerificationKey(String),
    #[error("downstream backpressure")]
    DownstreamBackpressure,
    #[error("server is shutting down")]
    ShuttingDown,
    // SYSCOIN: Disk/canonical-snapshot failures retain the exact lease and wrapper for replay.
    #[error("durable SNARK journal is temporarily unavailable: {0}")]
    DurableJournal(String),
    // SYSCOIN: Settlement ambiguity retains this exact lease; only canonical verifier rejection
    // is terminal and makes the range immediately repickable.
    #[error("settlement verifier preflight is temporarily unavailable")]
    VerifierPreflightUnavailable,
    #[error("settlement verifier rejected the SNARK proof")]
    ProofRejected,
    #[error("invalid or stale SNARK lease")]
    InvalidLease,
    // SYSCOIN: An overlapping replay of this same capability must wait, not discard its proof.
    #[error("this SNARK lease already has a submission in progress; retry later")]
    SubmissionInProgress,
    #[error("Verification key hash mismatch: server got {server}, prover got {prover}")]
    VerificationKeyMismatch { server: String, prover: String },
}

/// Job manager for SNARK proving.
///
/// Supports multiple SNARK provers
///
/// Supports both real and fake proofs.
///  - Fake FRI proofs always result in fake SNARK proofs.
///  - Real FRI proofs may result in real or fake SNARK proofs depending on prover availability
///
/// `SnarkJobManager` aims to assign real prover jobs to real SNARK provers -
///     but if jobs are not picked within a timeout (`max_batch_age`), it releases it to a fake prover
pub struct SnarkJobManager {
    // == state ==
    // SYSCOIN: Shared ownership lets RAII submission guards perform exact async cleanup.
    jobs: Arc<ProverJobMap<FriProof>>,
    // outbound
    prove_batches_sender: mpsc::Sender<ProofCommand>,
    // SYSCOIN: Real wrappers are fsynced here before jobs are consumed or HTTP returns 204.
    journal: Option<SnarkProofJournal>,
    // SYSCOIN: Production admission must pass the active on-chain verifier before the journal can
    // acquire durable authority over the range.
    preflight: Arc<dyn SnarkProofPreflight>,
    // SYSCOIN: A deterministically unpersistable oldest aggregate is node-critical, not a
    // request-local 500. Latch it once so the owning pipeline task terminates the node promptly.
    fatal_error: Arc<SnarkManagerFatalError>,
    // config
    // SYSCOIN: Amortize wrapping with a two-proof floor and target-or-age release policy.
    max_fris_per_snark: usize,
    target_fris_per_snark: usize,
    max_snark_batch_wait: Duration,
    #[cfg(test)]
    proof_decode_invocations: Arc<std::sync::atomic::AtomicUsize>,
    // SYSCOIN: Tests can pause the detached durable handoff after publication to prove that an
    // HTTP-future cancellation cannot become the live dispatcher's owner or strand the wrapper.
    #[cfg(test)]
    post_persist_handoff_gate: Option<Arc<PostPersistHandoffGate>>,
}

// SYSCOIN: One retained terminal fault supervises every detached durable handoff and forces the
// owning pipeline task to terminate instead of serving a partially live prover surface.
#[derive(Debug)]
struct SnarkManagerFatalError {
    sender: watch::Sender<Option<Arc<str>>>,
}

impl SnarkManagerFatalError {
    fn new() -> Self {
        let (sender, _receiver) = watch::channel(None);
        Self { sender }
    }

    fn latch(&self, message: String) {
        let message: Arc<str> = message.into();
        self.sender.send_if_modified(|current| {
            if current.is_some() {
                return false;
            }
            *current = Some(message);
            true
        });
    }

    fn current(&self) -> Option<Arc<str>> {
        self.sender.borrow().clone()
    }

    async fn wait(&self) -> Arc<str> {
        let mut receiver = self.sender.subscribe();
        loop {
            // SYSCOIN: `watch` retains the terminal value across every subscribe/check/await
            // interleaving, unlike a Notify waiter that has not yet been polled and enabled.
            if let Some(message) = receiver.borrow_and_update().clone() {
                return message;
            }
            receiver
                .changed()
                .await
                .expect("SNARK fatal-error sender is owned by the manager");
        }
    }
}

#[cfg(test)]
struct PostPersistHandoffGate {
    published: Semaphore,
    release: Semaphore,
}

#[cfg(test)]
impl PostPersistHandoffGate {
    fn new() -> Self {
        Self {
            published: Semaphore::new(0),
            release: Semaphore::new(0),
        }
    }
}

/// SYSCOIN: Exact SNARK aggregate plus the opaque capability required for submission.
pub struct LeasedSnarkJob {
    pub batches: Vec<(FriJob, FriProof)>,
    pub lease_token: String,
}

impl std::fmt::Debug for LeasedSnarkJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeasedSnarkJob")
            .field("batch_count", &self.batches.len())
            .field("lease_token", &"[REDACTED]")
            .finish()
    }
}

impl std::ops::Deref for LeasedSnarkJob {
    type Target = [(FriJob, FriProof)];

    fn deref(&self) -> &Self::Target {
        &self.batches
    }
}

impl SnarkJobManager {
    #[cfg(test)]
    pub fn new(
        prove_batches_sender: mpsc::Sender<ProofCommand>,
        max_fris_per_snark: usize,
        target_fris_per_snark: usize,
        max_snark_batch_wait: Duration,
        assignment_timeout: Duration,
        max_assigned_batch_range: usize,
    ) -> Self {
        Self::new_inner(
            prove_batches_sender,
            max_fris_per_snark,
            target_fris_per_snark,
            max_snark_batch_wait,
            assignment_timeout,
            max_assigned_batch_range,
            None,
            Arc::new(crate::prover_api::snark_proof_preflight::AcceptingTestSnarkProofPreflight),
        )
    }

    // SYSCOIN: Production construction requires a durable journal; tests that exercise only queue
    // semantics use the cfg-only constructor above and cannot create an unjournaled production node.
    // SYSCOIN: Keep durable recovery and verifier preflight explicit at the production constructor.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_journal(
        prove_batches_sender: mpsc::Sender<ProofCommand>,
        max_fris_per_snark: usize,
        target_fris_per_snark: usize,
        max_snark_batch_wait: Duration,
        assignment_timeout: Duration,
        max_assigned_batch_range: usize,
        journal: SnarkProofJournal,
        preflight: Arc<dyn SnarkProofPreflight>,
    ) -> Self {
        Self::new_inner(
            prove_batches_sender,
            max_fris_per_snark,
            target_fris_per_snark,
            max_snark_batch_wait,
            assignment_timeout,
            max_assigned_batch_range,
            Some(journal),
            preflight,
        )
    }

    // SYSCOIN: The shared constructor mirrors all independently validated prover pipeline bounds.
    #[allow(clippy::too_many_arguments)]
    fn new_inner(
        prove_batches_sender: mpsc::Sender<ProofCommand>,
        max_fris_per_snark: usize,
        target_fris_per_snark: usize,
        max_snark_batch_wait: Duration,
        assignment_timeout: Duration,
        max_assigned_batch_range: usize,
        journal: Option<SnarkProofJournal>,
        preflight: Arc<dyn SnarkProofPreflight>,
    ) -> Self {
        let jobs = Arc::new(ProverJobMap::<FriProof>::new(
            assignment_timeout,
            max_assigned_batch_range,
            ProverStage::Snark,
        ));
        Self {
            jobs,
            prove_batches_sender,
            journal,
            preflight,
            fatal_error: Arc::new(SnarkManagerFatalError::new()),
            max_fris_per_snark,
            target_fris_per_snark,
            max_snark_batch_wait,
            #[cfg(test)]
            proof_decode_invocations: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            #[cfg(test)]
            post_persist_handoff_gate: None,
        }
    }

    /// SYSCOIN: Live FRI admission enters the same ordered ownership map used by restart recovery.
    pub async fn add_job(
        &self,
        batch_envelope: SignedBatchEnvelope<FriProof>,
    ) -> SnarkJobAdmission {
        self.jobs.admit_snark_job(batch_envelope).await
    }

    /// SYSCOIN: Rehydrates a stored FRI proof without resetting the aggregation wait clock.
    pub async fn add_rehydrated_job(
        &self,
        batch_envelope: SignedBatchEnvelope<FriProof>,
        accepted_age: Duration,
    ) -> Result<SnarkJobAdmission, JobMapCapacityExceeded> {
        // Readiness only distinguishes ages below / above this threshold. Capping also keeps the
        // reconstructed monotonic instant representable even for unexpectedly ancient files.
        self.jobs
            .try_admit_snark_job_with_age(
                batch_envelope,
                accepted_age.min(self.max_snark_batch_wait),
            )
            .await
    }

    /// SYSCOIN: The startup owner seeds only settlement-canonical journal ranges before opening
    /// the listener, so later FRI replay cannot duplicate already-durable wrapping work.
    pub(crate) async fn seed_recovered_journal_ownership(
        &self,
        recovered_ranges: &[(u64, u64)],
    ) -> Result<(), SnarkOwnershipSeedError> {
        self.jobs
            .seed_snark_completed_ownership(recovered_ranges)
            .await
    }

    /// SYSCOIN: Install the immutable startup aggregate order before publishing Drainable.
    pub(crate) async fn install_startup_recovery_plan(
        &self,
        plan: StartupRecoveryPlan,
    ) -> Result<(), StartupRecoveryBoundaryError> {
        self.jobs.install_startup_recovery_plan(plan).await
    }

    /// SYSCOIN: Drainable recovery uses bounded live backpressure and preserves durable proof age;
    /// exact planned heads can complete concurrently and release capacity for the next range.
    pub(crate) async fn add_rehydrated_job_blocking(
        &self,
        batch_envelope: SignedBatchEnvelope<FriProof>,
        accepted_age: Duration,
    ) -> SnarkJobAdmission {
        self.jobs
            .admit_snark_job_with_age(batch_envelope, accepted_age.min(self.max_snark_batch_wait))
            .await
    }

    /// SYSCOIN: Loading completion does not expose live work while any planned range remains.
    pub(crate) async fn finish_startup_loading(&self) -> Result<(), StartupRecoveryBoundaryError> {
        self.jobs.finish_startup_loading().await
    }

    // SYSCOIN: The public real-prover picker applies ordered recovery and completed ownership
    // before returning a non-empty (`batch_number`, `verification_key_hash`, `real_fri_proof`) set.
    pub async fn pick_real_job(
        &self,
        prover_id: String,
        supported_proving_versions: Option<&[ProvingVersion]>,
    ) -> anyhow::Result<Option<LeasedSnarkJob>> {
        self.pick_real_job_with_response_limit(
            prover_id,
            supported_proving_versions,
            MAX_SNARK_PICK_RESPONSE_BYTES,
        )
        .await
    }

    // SYSCOIN: Keep the exact response boundary injectable for focused tests while production
    // always supplies the hard public HTTP cap above.
    async fn pick_real_job_with_response_limit(
        &self,
        prover_id: String,
        supported_proving_versions: Option<&[ProvingVersion]>,
        response_limit: usize,
    ) -> anyhow::Result<Option<LeasedSnarkJob>> {
        if let Some(message) = self.fatal_error.current() {
            anyhow::bail!("SNARK manager is terminally faulted: {message}");
        }
        // consume/remove all fake jobs that may be in the front of the queue
        self.process_pending_fake_fri_proofs().await?;

        let mut encoded_proof_bytes = 0_usize;
        let Some(proof_budget) = response_limit.checked_sub(SNARK_PICK_FIXED_JSON_BUDGET) else {
            let message = format!(
                "SNARK pick response limit {response_limit} is below the fixed \
                 {SNARK_PICK_FIXED_JSON_BUDGET}-byte JSON budget"
            );
            self.fatal_error.latch(message.clone());
            anyhow::bail!(message);
        };
        let pick = self
            .jobs
            .pick_ready_snark_jobs_with_response_capacity(
                self.max_fris_per_snark,
                self.target_fris_per_snark,
                self.max_snark_batch_wait,
                &prover_id,
                |job| {
                    // SYSCOIN: Only real FRI bytes are eligible for an external wrapper. The
                    // AlreadySubmittedToL1 passthrough marker is neither fake nor a proof. Stop
                    // the contiguous range before its exact encoded response could exceed cap.
                    if !supported_proving_versions
                        .is_none_or(|versions| versions.contains(&job.metadata.proving_version))
                    {
                        return SnarkJobEligibility::Incompatible;
                    }
                    if !matches!(job.batch_envelope.data, FriProof::Real(_)) {
                        return SnarkJobEligibility::Incompatible;
                    }
                    let Some(next_proof_bytes) =
                        snark_pick_proof_wire_bytes(&job.batch_envelope.data)
                    else {
                        // SYSCOIN: A real proof whose base64 arithmetic overflows is permanently
                        // above every representable response cap, never merely incompatible.
                        return SnarkJobEligibility::ResponseCapacityExceeded {
                            required_bytes: usize::MAX,
                            max_bytes: response_limit,
                        };
                    };
                    let Some(next_total) = encoded_proof_bytes.checked_add(next_proof_bytes) else {
                        return SnarkJobEligibility::ResponseCapacityExceeded {
                            required_bytes: usize::MAX,
                            max_bytes: response_limit,
                        };
                    };
                    if next_total > proof_budget {
                        return SnarkJobEligibility::ResponseCapacityExceeded {
                            required_bytes: SNARK_PICK_FIXED_JSON_BUDGET.saturating_add(next_total),
                            max_bytes: response_limit,
                        };
                    }
                    encoded_proof_bytes = next_total;
                    SnarkJobEligibility::Eligible
                },
            )
            .await;
        match pick {
            SnarkJobPick::Assigned { jobs, lease_token } => Ok(Some(LeasedSnarkJob {
                batches: jobs,
                lease_token,
            })),
            SnarkJobPick::Waiting(wait) => {
                tracing::trace!(
                    prover_id,
                    eligible_fris = wait.eligible_fris,
                    minimum_fris = 2,
                    target_fris = self.target_fris_per_snark,
                    oldest_eligible_age_seconds = wait.oldest_eligible_age.as_secs(),
                    max_wait_seconds = self.max_snark_batch_wait.as_secs(),
                    "SNARK proofs are queued but intentionally waiting for the two-proof minimum and target, age, or interop readiness",
                );
                Ok(None)
            }
            SnarkJobPick::Unpersistable {
                batch_from,
                blocked_at,
                required_bytes,
                max_bytes,
            } => {
                // SYSCOIN: No lease exists at this point. Latch the deterministic local-storage
                // incompatibility so the critical pipeline cannot silently poll forever.
                let message = format!(
                    "oldest contiguous SNARK aggregate {batch_from}-{blocked_at} requires at \
                     least {required_bytes} durable journal bytes, above the {max_bytes}-byte hard cap"
                );
                self.fatal_error.latch(message.clone());
                anyhow::bail!("{message}")
            }
            SnarkJobPick::UnservableResponse {
                batch_from,
                blocked_at,
                required_bytes,
                max_bytes,
            } => {
                // SYSCOIN: No lease exists at this point. A canonical two-FRI response that cannot
                // cross the configured wire cap is an operator-visible terminal configuration.
                let message = format!(
                    "oldest contiguous SNARK aggregate {batch_from}-{blocked_at} requires at \
                     least {required_bytes} response bytes, above the {max_bytes}-byte hard cap"
                );
                self.fatal_error.latch(message.clone());
                anyhow::bail!("{message}")
            }
            SnarkJobPick::Unwrappable {
                batch_from,
                batch_to,
                fittable_fris,
            } => {
                // SYSCOIN: No lease exists; advancing would strand an interior singleton and
                // jumping a later planned range would violate settlement order.
                let message = format!(
                    "planned startup SNARK range {batch_from}-{batch_to} fits only \
                     {fittable_fris} FRIs and would strand an interior singleton"
                );
                self.fatal_error.latch(message.clone());
                anyhow::bail!("{message}")
            }
            SnarkJobPick::Empty => {
                tracing::trace!(prover_id, "no SNARK prove jobs are available for pick up",);
                Ok(None)
            }
        }
    }

    pub async fn submit_proof(
        &self,
        batch_from: u64,
        batch_to: u64,
        vk_hash: String,
        encoded_payload: String,
        prover_id: String,
        lease_token: String,
    ) -> Result<(), SnarkSubmitError> {
        // SYSCOIN: Reject malformed external SNARK submit ranges before touching job state.
        if batch_from > batch_to {
            return Err(SnarkSubmitError::InvalidRange {
                from: batch_from,
                to: batch_to,
            });
        }
        // SYSCOIN: A public prover label/range is diagnostic only. The random pick capability is
        // the authority, and admission precedes proof decoding so an authenticated caller without
        // the exact lease cannot create unbounded concurrent base64 work.
        let submission = self
            .jobs
            .begin_submission(batch_from, batch_to, &lease_token)
            .await
            .map_err(|error| match error {
                BeginSubmissionError::AlreadySubmitting => SnarkSubmitError::SubmissionInProgress,
                _ => SnarkSubmitError::InvalidLease,
            })?;

        // SYSCOIN: Reserve transient downstream/preflight capacity before parsing or decoding the
        // capability owner's body. A full queue releases only `submitting`, retaining the same
        // lease while ensuring every automatic 429 retry performs zero repeated decode work.
        let permit = match self.prove_batches_sender.clone().try_reserve_owned() {
            Ok(permit) => permit,
            Err(TrySendError::Full(_)) => {
                submission.release_for_retry().await;
                return Err(SnarkSubmitError::DownstreamBackpressure);
            }
            Err(TrySendError::Closed(_)) => {
                submission.release_for_retry().await;
                return Err(SnarkSubmitError::ShuttingDown);
            }
        };

        // SYSCOIN: Malformed data from the authenticated capability owner is definitive. Revoke
        // exactly that lease so the range is immediately repickable; stale capabilities never
        // reach any decoding or version-selection work.
        let proving_version = match ProvingVersion::try_from_vk_hash(&vk_hash) {
            Ok(proving_version) => proving_version,
            Err(error) => {
                submission.revoke().await;
                return Err(SnarkSubmitError::UnknownVerificationKey(error.to_string()));
            }
        };
        #[cfg(test)]
        self.proof_decode_invocations
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let payload = match general_purpose::STANDARD.decode(encoded_payload) {
            Ok(payload) => payload,
            Err(error) => {
                submission.revoke().await;
                return Err(SnarkSubmitError::InvalidBase64(error.to_string()));
            }
        };
        // SYSCOIN: The pinned V32 verifier consumes exactly 44 prover-supplied words. Reject every
        // other shape before an oversized verifier eth_call can time out and keep the earliest
        // aggregate leased indefinitely; revoke only the exact admitted capability.
        if payload.is_empty() {
            submission.revoke().await;
            return Err(SnarkSubmitError::EmptyProof);
        }
        if payload.len() != ZKSYNC_OS_V8_REAL_PROOF_BYTES {
            let length = payload.len();
            submission.revoke().await;
            return Err(SnarkSubmitError::InvalidProofLength(length));
        }

        let prover_vk = proving_version.vk_hash();
        // SYSCOIN: Validate every metadata snapshot admitted under the token. Never reread mutable
        // map state or assume the first batch represents a range if an internal invariant regresses.
        let mismatched_server_vk = submission.proving_versions().find_map(|server_version| {
            let server_vk = server_version.vk_hash();
            (server_vk != prover_vk).then(|| server_vk.to_owned())
        });
        if let Some(server_vk) = mismatched_server_vk {
            // SYSCOIN: A definitive authenticated-owner mismatch revokes only this exact token so
            // the range is immediately repickable. Transient internal/RPC failures must instead
            // let the RAII guard release in-progress while retaining the assignment.
            submission.revoke().await;
            return Err(SnarkSubmitError::VerificationKeyMismatch {
                server: server_vk,
                prover: prover_vk.to_owned(),
            });
        }

        let snark_proof = SnarkProof::Real(RealSnarkProof {
            proof: payload,
            proving_execution_version: proving_version as u32,
        });

        // SYSCOIN: Snapshot canonical metadata while this exact token remains in progress, then
        // preflight the identical Executor/wrapper inputs before fsync publication or job removal.
        let Some(durable_batches) = submission.durable_snark_batches().await else {
            submission.release_for_retry().await;
            return Err(SnarkSubmitError::DurableJournal(
                "exact leased metadata changed before verifier preflight".to_owned(),
            ));
        };
        let durable_batches: Vec<_> = durable_batches
            .into_iter()
            .map(|batch| {
                BatchEnvelope::new(batch, FriProof::AlreadySubmittedToL1)
                    // SYSCOIN: These batches are already canonical on the settlement layer.
                    // Do not persist or later trust obsolete commit-signature bytes; the typed
                    // marker is all prove/execute downstream stages require.
                    .with_signatures(BatchSignatureData::AlreadyCommitted)
                    .with_stage(BatchExecutionStage::SnarkProvedReal)
            })
            .collect();
        match self.preflight.verify(&durable_batches, &snark_proof).await {
            Ok(()) => {}
            Err(SnarkProofPreflightError::Unavailable) => {
                submission.release_for_retry().await;
                return Err(SnarkSubmitError::VerifierPreflightUnavailable);
            }
            Err(SnarkProofPreflightError::Rejected) => {
                submission.revoke().await;
                return Err(SnarkSubmitError::ProofRejected);
            }
        }

        if let Some(journal) = &self.journal {
            let journal = journal.clone();
            let fatal_error = Arc::clone(&self.fatal_error);
            #[cfg(test)]
            let post_persist_handoff_gate = self.post_persist_handoff_gate.clone();

            // SYSCOIN: Once verifier preflight succeeds, a detached task owns the exact lease,
            // downstream capacity, publication, consumption, and enqueue as one live handoff.
            // Dropping the HTTP request future only detaches its JoinHandle; it cannot cancel the
            // task between fsync publication and command dispatch or leave replay for restart.
            let handoff = tokio::spawn(async move {
                let journaled = match journal.persist(durable_batches, snark_proof).await {
                    Ok(journaled) => journaled,
                    Err(error) => {
                        submission.release_for_retry().await;
                        return Err(SnarkSubmitError::DurableJournal(error.to_string()));
                    }
                };

                #[cfg(test)]
                if let Some(gate) = post_persist_handoff_gate {
                    gate.published.add_permits(1);
                    gate.release
                        .acquire()
                        .await
                        .expect("test post-persist handoff gate must remain open")
                        .forget();
                }

                // SYSCOIN: The fsynced record now owns crash recovery. Only then consume the exact
                // jobs and enqueue a command carrying its post-confirmation cleanup capability.
                match submission
                    .complete_with_snark_ownership(
                        SnarkCompletedOwner::JournalOwned,
                        ProverType::Real,
                        &prover_id,
                    )
                    .await
                {
                    SnarkOwnershipCompletion::Completed(_) => {}
                    SnarkOwnershipCompletion::AlreadyOwned => {
                        let message = "durably published SNARK range unexpectedly already had completed ownership".to_owned();
                        fatal_error.latch(message.clone());
                        return Err(SnarkSubmitError::DurableJournal(message));
                    }
                    SnarkOwnershipCompletion::Stale => {
                        let message =
                            "exact leased jobs changed after durable SNARK publication".to_owned();
                        fatal_error.latch(message.clone());
                        return Err(SnarkSubmitError::DurableJournal(message));
                    }
                }
                permit.send(journaled.into_command(journal.confirmation_sender()));
                Ok(())
            });

            return handoff.await.map_err(|error| {
                SnarkSubmitError::DurableJournal(format!(
                    "durable SNARK handoff task failed: {error}"
                ))
            })?;
        }
        // SYSCOIN: This branch is compiled only for focused unit tests; production construction
        // requires `new_with_journal` and can never acknowledge an unjournaled real wrapper.
        #[cfg(test)]
        {
            let consumed_batches = match submission
                .complete_with_snark_ownership(
                    SnarkCompletedOwner::CommandOwned,
                    ProverType::Real,
                    &prover_id,
                )
                .await
            {
                SnarkOwnershipCompletion::Completed(consumed) => consumed,
                SnarkOwnershipCompletion::AlreadyOwned | SnarkOwnershipCompletion::Stale => {
                    return Err(SnarkSubmitError::InvalidLease);
                }
            };
            let consumed_batches = consumed_batches
                .into_iter()
                .map(|batch| batch.with_stage(BatchExecutionStage::SnarkProvedReal))
                .collect();
            permit.send(ProofCommand::new(consumed_batches, snark_proof));
            Ok(())
        }
        #[cfg(not(test))]
        unreachable!("production SNARK job manager must have a durable journal");
    }

    /// Consumes fake FRI proofs from the head of the queue and turns them into fake SNARKs.
    async fn process_pending_fake_fri_proofs(&self) -> anyhow::Result<()> {
        self.process_pending_fake_or_timed_out_fri_proofs(None)
            .await
    }

    /// Consumes FRI proofs from the head of the queue that satisfy the following conditions:
    /// * FRI proof is fake
    /// * if `timeout_for_real_fris` is Some, then also jobs that are older than `timeout_for_real_fris`
    async fn process_pending_fake_or_timed_out_fri_proofs(
        &self,
        timeout_for_real_fris: Option<Duration>,
    ) -> anyhow::Result<()> {
        loop {
            let is_fake_or_timed_out = |job: &JobEntry<FriProof>| {
                job.batch_envelope.data.is_fake()
                    || timeout_for_real_fris
                        .is_some_and(|timeout| job.metadata.added_at.elapsed() >= timeout)
            };
            if !self.jobs.has_assignable_job(is_fake_or_timed_out).await {
                return Ok(());
            }

            let permit = self.try_reserve_permit_downstream()?;
            let Some(leased) = self
                .jobs
                .pick_leased_jobs_while_with_limit(
                    self.max_fris_per_snark,
                    "fake_prover",
                    is_fake_or_timed_out,
                )
                .await
            else {
                return Ok(());
            };
            let assigned = leased.jobs;
            let real_proofs_count = assigned
                .iter()
                .filter(|(_, proof)| !proof.is_fake())
                .count();
            tracing::info!(
                "consuming fake proofs for SNARKing for batches {}-{} ({} real proofs; {} fake proofs)",
                assigned.first().unwrap().0.batch_number,
                assigned.last().unwrap().0.batch_number,
                real_proofs_count,
                assigned.len() - real_proofs_count,
            );

            let batch_from = assigned.first().unwrap().0.batch_number;
            let batch_to = assigned.last().unwrap().0.batch_number;
            let Some(completed) = self
                .complete_fake_leased_jobs(batch_from, batch_to, "fake_prover", &leased.lease_token)
                .await
            else {
                tracing::info!(
                    batch_from,
                    batch_to,
                    "skipping stale fake SNARK lease after range reassignment"
                );
                continue;
            };

            // Add observability traces
            let batches_with_fake_proofs = completed
                .into_iter()
                .map(|batch| batch.with_stage(BatchExecutionStage::SnarkProvedFake))
                .collect();

            // SYSCOIN: Fake wrappers are deterministic, immediate, and reconstructible from the
            // still-durable fake FRI inputs, so they do not consume the real-proof journal.
            permit.send(ProofCommand::new(
                batches_with_fake_proofs,
                SnarkProof::Fake,
            ));
        }
    }

    /// SYSCOIN: A delayed fake aggregate may complete only the exact capability it picked; its
    /// public label and range cannot consume a newer real-wrapper assignment.
    async fn complete_fake_leased_jobs(
        &self,
        batch_from: u64,
        batch_to: u64,
        prover_id: &str,
        lease_token: &str,
    ) -> Option<Vec<SignedBatchEnvelope<FriProof>>> {
        let submission = self
            .jobs
            .begin_submission(batch_from, batch_to, lease_token)
            .await
            .ok()?;
        match submission
            .complete_with_snark_ownership(
                SnarkCompletedOwner::CommandOwned,
                ProverType::Fake,
                prover_id,
            )
            .await
        {
            SnarkOwnershipCompletion::Completed(completed) => Some(completed),
            SnarkOwnershipCompletion::AlreadyOwned | SnarkOwnershipCompletion::Stale => None,
        }
    }

    fn try_reserve_permit_downstream(&self) -> anyhow::Result<Permit<'_, ProofCommand>> {
        Ok(match self.prove_batches_sender.try_reserve() {
            Ok(permit) => permit,
            Err(TrySendError::Full(_)) => {
                anyhow::bail!("downstream backpressure");
            }
            Err(TrySendError::Closed(_)) => {
                anyhow::bail!("server is shutting down");
            }
        })
    }

    // SYSCOIN: Expose aggregate queue state for multi-worker prover orchestration.
    pub async fn status(&self) -> Vec<JobState> {
        self.jobs.status().await
    }

    // SYSCOIN: The pipeline owns this future and treats its first value as a critical component
    // error. The OnceLock also makes late subscribers observe the same terminal fault immediately.
    pub(crate) async fn wait_for_fatal_error(&self) -> Arc<str> {
        self.fatal_error.wait().await
    }
}

const POLL_INTERVAL_MS: u64 = 1000;

pub struct FakeSnarkProver {
    job_manager: Arc<SnarkJobManager>,

    // config
    max_batch_age: Duration,
    polling_interval: Duration,
}

impl FakeSnarkProver {
    pub fn new(job_manager: Arc<SnarkJobManager>, max_batch_age: Duration) -> Self {
        Self {
            job_manager,
            max_batch_age,
            polling_interval: Duration::from_millis(POLL_INTERVAL_MS),
        }
    }

    pub async fn run(self) {
        loop {
            tokio::time::sleep(self.polling_interval).await;
            if let Err(err) = self
                .job_manager
                .process_pending_fake_or_timed_out_fri_proofs(Some(self.max_batch_age))
                .await
            {
                tracing::info!("`FakeSnarkProver` iteration failed: {err}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prover_api::snark_proof_journal::{MAX_JOURNAL_RECORD_BYTES, SnarkProofJournal};
    use crate::prover_api::snark_proof_preflight::SnarkProofPreflightError;
    use crate::prover_api::test_util::{
        create_test_batch_envelope_with_data, mark_test_batch_as_interop_bundle,
    };
    use alloy::primitives::Bytes;
    use std::collections::VecDeque;
    use tempfile::TempDir;
    use zksync_os_batch_types::batcher_model::RealFriProof;
    use zksync_os_l1_sender::commands::SendToL1 as _;
    use zksync_os_types::{ProtocolSemanticVersion, ProvingVersion};

    fn real_fri_proof() -> FriProof {
        FriProof::Real(RealFriProof {
            proof: Bytes::from_static(b"stored-fri-proof"),
            proving_execution_version: ProvingVersion::V8 as u32,
        })
    }

    fn v8_vk_hash() -> String {
        ProvingVersion::V8.vk_hash().to_owned()
    }

    fn encoded_snark_proof(length: usize) -> String {
        general_purpose::STANDARD.encode(vec![0; length])
    }

    // SYSCOIN: Script exact verifier outcomes without weakening production construction or
    // coupling lease-disposition tests to a deployed EVM.
    struct SequencedPreflight {
        results: tokio::sync::Mutex<VecDeque<Result<(), SnarkProofPreflightError>>>,
    }

    impl SequencedPreflight {
        fn new(results: impl IntoIterator<Item = Result<(), SnarkProofPreflightError>>) -> Self {
            Self {
                results: tokio::sync::Mutex::new(results.into_iter().collect()),
            }
        }
    }

    #[async_trait::async_trait]
    impl SnarkProofPreflight for SequencedPreflight {
        async fn verify(
            &self,
            _batches: &[SignedBatchEnvelope<FriProof>],
            _proof: &SnarkProof,
        ) -> Result<(), SnarkProofPreflightError> {
            self.results
                .lock()
                .await
                .pop_front()
                .expect("scripted preflight result")
        }
    }

    async fn add_two_contiguous_real_jobs(manager: &SnarkJobManager) -> Vec<BatchMetadata> {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let mut previous = None;
        let mut batches = Vec::new();
        for batch_number in 1..=2 {
            let mut batch = create_test_batch_envelope_with_data(
                batch_number,
                protocol_version.clone(),
                real_fri_proof(),
            );
            if let Some(previous) = previous {
                batch.batch.previous_stored_batch_info = previous;
            }
            previous = Some(batch.batch.batch_info.clone().into_stored());
            batches.push(batch.batch.clone());
            assert_eq!(manager.add_job(batch).await, SnarkJobAdmission::Inserted);
        }
        batches
    }

    fn job_from_metadata(batch: &BatchMetadata, proof: FriProof) -> SignedBatchEnvelope<FriProof> {
        BatchEnvelope::new(batch.clone(), proof).with_signatures(BatchSignatureData::NotNeeded)
    }

    // SYSCOIN: Ownership checks must not weaken the fail-closed authoritative-metadata invariant
    // for a same-number replay that still has a live queue entry.
    #[tokio::test]
    #[should_panic(expected = "conflicting same-number prover job metadata")]
    async fn conflicting_replay_fails_before_completed_ownership_check() {
        let (sender, _receiver) = mpsc::channel(1);
        let manager =
            SnarkJobManager::new(sender, 2, 2, Duration::ZERO, Duration::from_secs(60), 16);
        let metadata = add_two_contiguous_real_jobs(&manager).await.remove(0);
        let mut conflicting = job_from_metadata(&metadata, real_fri_proof());
        conflicting.batch.tx_count += 1;
        manager.add_job(conflicting).await;
    }

    // SYSCOIN: Recovery seeds every validated durable range before Drainable and coalesced
    // tombstones exclude all later recreated-pipeline copies without consuming map capacity.
    #[tokio::test]
    async fn recovered_journal_ownership_excludes_replayed_fris() -> anyhow::Result<()> {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let (sender, _receiver) = mpsc::channel(1);
        let manager =
            SnarkJobManager::new(sender, 2, 2, Duration::ZERO, Duration::from_secs(60), 1);
        manager
            .seed_recovered_journal_ownership(&[(1, 2), (3, 4)])
            .await?;
        for batch_number in [1, 4] {
            assert_eq!(
                manager
                    .add_job(create_test_batch_envelope_with_data(
                        batch_number,
                        protocol_version.clone(),
                        real_fri_proof(),
                    ))
                    .await,
                SnarkJobAdmission::AlreadyOwned
            );
        }
        assert_eq!(
            manager
                .add_job(create_test_batch_envelope_with_data(
                    10,
                    protocol_version,
                    real_fri_proof(),
                ))
                .await,
            SnarkJobAdmission::Inserted
        );
        assert_eq!(manager.status().await.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn real_submission_is_journaled_before_jobs_are_consumed_and_ack_reaps_it()
    -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let (journal, confirmations) = SnarkProofJournal::open(temp.path()).await?;
        let (sender, mut receiver) = mpsc::channel(1);
        let manager = SnarkJobManager::new_with_journal(
            sender,
            2,
            2,
            Duration::ZERO,
            Duration::from_secs(60),
            16,
            journal.clone(),
            Arc::new(crate::prover_api::snark_proof_preflight::AcceptingTestSnarkProofPreflight),
        );
        let completed_inputs = add_two_contiguous_real_jobs(&manager).await;
        let picked = manager
            .pick_real_job("durable-wrapper".to_owned(), Some(&[ProvingVersion::V8]))
            .await?
            .expect("two-proof aggregate must be ready");
        manager
            .submit_proof(
                1,
                2,
                v8_vk_hash(),
                encoded_snark_proof(ZKSYNC_OS_V8_REAL_PROOF_BYTES),
                "durable-wrapper".to_owned(),
                picked.lease_token,
            )
            .await?;

        assert_eq!(journal.record_count().await, 1);
        assert!(manager.status().await.is_empty());
        let command = receiver.recv().await.expect("journaled proof command");

        let reaper = tokio::spawn(journal.clone().run_reaper(confirmations));
        command.notify_confirmed();
        tokio::time::timeout(Duration::from_secs(2), async {
            while journal.record_count().await != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        reaper.abort();
        for batch in completed_inputs {
            assert_eq!(
                manager
                    .add_job(job_from_metadata(&batch, real_fri_proof()))
                    .await,
                SnarkJobAdmission::AlreadyOwned,
                "journal reaping must not remove the in-process completed tombstone"
            );
        }
        assert!(manager.status().await.is_empty());
        Ok(())
    }

    // SYSCOIN: An HTTP disconnect after fsync publication must detach only the waiter. The owned
    // live handoff continues consuming the exact lease and dispatches exactly one durable command.
    #[tokio::test]
    async fn request_cancellation_after_publication_does_not_strand_durable_handoff()
    -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let (journal, _confirmations) = SnarkProofJournal::open(temp.path()).await?;
        let (sender, mut receiver) = mpsc::channel(1);
        let gate = Arc::new(PostPersistHandoffGate::new());
        let mut manager = SnarkJobManager::new_with_journal(
            sender,
            2,
            2,
            Duration::ZERO,
            Duration::from_secs(60),
            16,
            journal.clone(),
            Arc::new(crate::prover_api::snark_proof_preflight::AcceptingTestSnarkProofPreflight),
        );
        manager.post_persist_handoff_gate = Some(gate.clone());
        let manager = Arc::new(manager);
        let replayed_inputs = add_two_contiguous_real_jobs(&manager).await;
        let picked = manager
            .pick_real_job(
                "disconnecting-wrapper".to_owned(),
                Some(&[ProvingVersion::V8]),
            )
            .await?
            .expect("two-proof aggregate must be ready");

        let submitting_manager = manager.clone();
        let submitter = tokio::spawn(async move {
            submitting_manager
                .submit_proof(
                    1,
                    2,
                    v8_vk_hash(),
                    encoded_snark_proof(ZKSYNC_OS_V8_REAL_PROOF_BYTES),
                    "disconnecting-wrapper".to_owned(),
                    picked.lease_token,
                )
                .await
        });

        gate.published.acquire().await?.forget();
        assert_eq!(journal.record_count().await, 1);
        // SYSCOIN: Publication has happened but completion is gated. A recreated-pipeline replay
        // must still see each live entry as an idempotent duplicate, never fresh work.
        for batch in &replayed_inputs {
            assert_eq!(
                manager
                    .add_job(job_from_metadata(batch, real_fri_proof()))
                    .await,
                SnarkJobAdmission::Duplicate
            );
        }
        submitter.abort();
        let _ = submitter.await;
        gate.release.add_permits(1);

        let _command = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
            .await?
            .expect("detached durable handoff must dispatch without restart");
        assert!(manager.status().await.is_empty());
        assert!(
            receiver.try_recv().is_err(),
            "handoff dispatched more than once"
        );
        for batch in replayed_inputs {
            assert_eq!(
                manager
                    .add_job(job_from_metadata(&batch, real_fri_proof()))
                    .await,
                SnarkJobAdmission::AlreadyOwned
            );
        }
        assert!(manager.status().await.is_empty());
        Ok(())
    }

    // SYSCOIN: Once fsync publishes recovery authority, failure to consume the exact capability is
    // a node-critical invariant break. It must latch fatal instead of leaving the API live.
    #[tokio::test]
    async fn post_persist_completion_failure_latches_fatal() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let (journal, _confirmations) = SnarkProofJournal::open(temp.path()).await?;
        let (sender, _receiver) = mpsc::channel(1);
        let gate = Arc::new(PostPersistHandoffGate::new());
        let mut manager = SnarkJobManager::new_with_journal(
            sender,
            2,
            2,
            Duration::ZERO,
            Duration::from_secs(60),
            16,
            journal,
            Arc::new(crate::prover_api::snark_proof_preflight::AcceptingTestSnarkProofPreflight),
        );
        manager.post_persist_handoff_gate = Some(gate.clone());
        let manager = Arc::new(manager);
        let _ = add_two_contiguous_real_jobs(&manager).await;
        let picked = manager
            .pick_real_job("faulted-wrapper".to_owned(), Some(&[ProvingVersion::V8]))
            .await?
            .expect("two-proof aggregate must be ready");

        let submitting_manager = Arc::clone(&manager);
        let submitter = tokio::spawn(async move {
            submitting_manager
                .submit_proof(
                    1,
                    2,
                    v8_vk_hash(),
                    encoded_snark_proof(ZKSYNC_OS_V8_REAL_PROOF_BYTES),
                    "faulted-wrapper".to_owned(),
                    picked.lease_token,
                )
                .await
        });
        gate.published.acquire().await?.forget();
        {
            let mut jobs = manager.jobs.lock_jobs_for_test().await;
            jobs.remove(&2)
                .expect("test must invalidate exact completion");
        }
        gate.release.add_permits(1);

        assert!(matches!(
            submitter.await?,
            Err(SnarkSubmitError::DurableJournal(_))
        ));
        let fatal =
            tokio::time::timeout(Duration::from_secs(1), manager.wait_for_fatal_error()).await?;
        assert!(fatal.contains("changed after durable SNARK publication"));
        Ok(())
    }

    #[tokio::test]
    async fn journal_io_failure_retains_exact_lease_for_identical_retry() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let (journal, _confirmations) = SnarkProofJournal::open(temp.path()).await?;
        let (sender, mut receiver) = mpsc::channel(1);
        let manager = SnarkJobManager::new_with_journal(
            sender,
            2,
            2,
            Duration::ZERO,
            Duration::from_secs(60),
            16,
            journal.clone(),
            Arc::new(crate::prover_api::snark_proof_preflight::AcceptingTestSnarkProofPreflight),
        );
        let _ = add_two_contiguous_real_jobs(&manager).await;
        let picked = manager
            .pick_real_job("io-retry-wrapper".to_owned(), Some(&[ProvingVersion::V8]))
            .await?
            .expect("two-proof aggregate must be ready");
        let lease_token = picked.lease_token;
        journal.fail_next_persist_for_test();

        let first = manager
            .submit_proof(
                1,
                2,
                v8_vk_hash(),
                encoded_snark_proof(ZKSYNC_OS_V8_REAL_PROOF_BYTES),
                "io-retry-wrapper".to_owned(),
                lease_token.clone(),
            )
            .await;
        assert!(matches!(first, Err(SnarkSubmitError::DurableJournal(_))));
        assert_eq!(journal.record_count().await, 0);
        assert!(
            manager
                .pick_real_job("competing-wrapper".to_owned(), Some(&[ProvingVersion::V8]))
                .await?
                .is_none(),
            "transient journal failure must retain the original capability"
        );

        manager
            .submit_proof(
                1,
                2,
                v8_vk_hash(),
                encoded_snark_proof(ZKSYNC_OS_V8_REAL_PROOF_BYTES),
                "io-retry-wrapper".to_owned(),
                lease_token,
            )
            .await?;
        assert_eq!(journal.record_count().await, 1);
        assert!(manager.status().await.is_empty());
        assert!(receiver.recv().await.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn impossible_journal_pair_latches_terminal_pipeline_fault_without_a_lease()
    -> anyhow::Result<()> {
        let (sender, _receiver) = mpsc::channel(1);
        let manager = Arc::new(SnarkJobManager::new(
            sender,
            100,
            100,
            Duration::ZERO,
            Duration::from_secs(60),
            16,
        ));
        let _ = add_two_contiguous_real_jobs(&manager).await;
        {
            let mut jobs = manager.jobs.lock_jobs_for_test().await;
            for entry in jobs.values_mut() {
                // Each single contribution fits; the mandatory two-FRI record does not.
                entry.metadata.durable_snark_batch_json_bytes = MAX_JOURNAL_RECORD_BYTES / 2;
            }
        }

        let waiting_manager = manager.clone();
        let fatal_waiter =
            tokio::spawn(async move { waiting_manager.wait_for_fatal_error().await });
        tokio::task::yield_now().await;

        let first_error = manager
            .pick_real_job("fatal-wrapper".to_owned(), Some(&[ProvingVersion::V8]))
            .await
            .unwrap_err()
            .to_string();
        assert!(first_error.contains("above the"));
        let latched = tokio::time::timeout(Duration::from_secs(1), fatal_waiter).await??;
        assert!(first_error.contains(latched.as_ref()));
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), manager.wait_for_fatal_error()).await?,
            latched,
            "late critical-task subscribers must observe the same retained fault"
        );

        let repeated = manager
            .pick_real_job("later-wrapper".to_owned(), Some(&[ProvingVersion::V8]))
            .await
            .unwrap_err()
            .to_string();
        assert!(repeated.contains(latched.as_ref()));
        assert!(
            manager
                .jobs
                .lock_jobs_for_test()
                .await
                .values()
                .all(|entry| entry.metadata.current_attempt == 0),
            "terminal capacity detection must happen before lease creation or refresh"
        );
        Ok(())
    }

    // SYSCOIN: RPC/topology ambiguity must retain the exact capability and publish nothing; an
    // identical retry can later pass preflight and become durable without re-picking the range.
    #[tokio::test]
    async fn unavailable_preflight_retains_exact_lease_and_journals_only_after_retry()
    -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let (journal, _confirmations) = SnarkProofJournal::open(temp.path()).await?;
        let (sender, _receiver) = mpsc::channel(1);
        let manager = SnarkJobManager::new_with_journal(
            sender,
            2,
            2,
            Duration::ZERO,
            Duration::from_secs(60),
            16,
            journal.clone(),
            Arc::new(SequencedPreflight::new([
                Err(SnarkProofPreflightError::Unavailable),
                Ok(()),
            ])),
        );
        let _ = add_two_contiguous_real_jobs(&manager).await;
        let picked = manager
            .pick_real_job("retrying-wrapper".to_owned(), Some(&[ProvingVersion::V8]))
            .await?
            .expect("two-proof aggregate");
        let token = picked.lease_token;

        assert!(matches!(
            manager
                .submit_proof(
                    1,
                    2,
                    v8_vk_hash(),
                    encoded_snark_proof(ZKSYNC_OS_V8_REAL_PROOF_BYTES),
                    "retrying-wrapper".to_owned(),
                    token.clone(),
                )
                .await,
            Err(SnarkSubmitError::VerifierPreflightUnavailable)
        ));
        assert_eq!(journal.record_count().await, 0);
        assert_eq!(manager.status().await.len(), 2);

        manager
            .submit_proof(
                1,
                2,
                v8_vk_hash(),
                encoded_snark_proof(ZKSYNC_OS_V8_REAL_PROOF_BYTES),
                "retrying-wrapper".to_owned(),
                token,
            )
            .await?;
        assert_eq!(journal.record_count().await, 1);
        assert!(manager.status().await.is_empty());
        Ok(())
    }

    // SYSCOIN: A canonical verifier rejection revokes only the admitted capability, leaves no
    // journal authority behind, and makes the unchanged FRI range immediately repickable.
    #[tokio::test]
    async fn rejected_preflight_revokes_lease_without_journaling() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let (journal, _confirmations) = SnarkProofJournal::open(temp.path()).await?;
        let (sender, _receiver) = mpsc::channel(1);
        let manager = SnarkJobManager::new_with_journal(
            sender,
            2,
            2,
            Duration::ZERO,
            Duration::from_secs(60),
            16,
            journal.clone(),
            Arc::new(SequencedPreflight::new([Err(
                SnarkProofPreflightError::Rejected,
            )])),
        );
        let _ = add_two_contiguous_real_jobs(&manager).await;
        let picked = manager
            .pick_real_job("invalid-wrapper".to_owned(), Some(&[ProvingVersion::V8]))
            .await?
            .expect("two-proof aggregate");
        let old_token = picked.lease_token;

        assert!(matches!(
            manager
                .submit_proof(
                    1,
                    2,
                    v8_vk_hash(),
                    encoded_snark_proof(ZKSYNC_OS_V8_REAL_PROOF_BYTES),
                    "invalid-wrapper".to_owned(),
                    old_token.clone(),
                )
                .await,
            Err(SnarkSubmitError::ProofRejected)
        ));
        assert_eq!(journal.record_count().await, 0);
        let repicked = manager
            .pick_real_job(
                "replacement-wrapper".to_owned(),
                Some(&[ProvingVersion::V8]),
            )
            .await?
            .expect("rejected range must be repickable");
        assert_ne!(repicked.lease_token, old_token);
        Ok(())
    }

    // SYSCOIN: The pre-lease response budget uses the exact base64 quantum plus JSON element
    // framing and rejects every non-real pipeline marker.
    #[test]
    fn snark_pick_wire_size_is_checked_and_exact() {
        let proof = FriProof::Real(RealFriProof {
            proof: Bytes::from_static(&[1, 2, 3, 4]),
            proving_execution_version: ProvingVersion::V8 as u32,
        });
        assert_eq!(snark_pick_proof_wire_bytes(&proof), Some(11));
        assert_eq!(snark_pick_proof_wire_bytes(&FriProof::Fake), None);
        assert_eq!(
            snark_pick_proof_wire_bytes(&FriProof::AlreadySubmittedToL1),
            None
        );
    }

    // SYSCOIN: Crossing the response cap after a valid two-FRI prefix is an immediate split
    // boundary even when neither the target nor max-wait threshold has been reached.
    #[tokio::test]
    async fn response_capacity_releases_two_proof_prefix_without_waiting() -> anyhow::Result<()> {
        const TEST_PROOF_BYTES: usize = 5_000;

        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let (sender, _receiver) = mpsc::channel(1);
        let manager = SnarkJobManager::new(
            sender,
            100,
            100,
            Duration::from_secs(3600),
            Duration::from_secs(60),
            100,
        );
        let mut previous = None;
        for batch_number in 1..=3 {
            let mut batch = create_test_batch_envelope_with_data(
                batch_number,
                protocol_version.clone(),
                FriProof::Real(RealFriProof {
                    proof: Bytes::from(vec![0x5a; TEST_PROOF_BYTES]),
                    proving_execution_version: ProvingVersion::V8 as u32,
                }),
            );
            if let Some(previous) = previous {
                batch.batch.previous_stored_batch_info = previous;
            }
            previous = Some(batch.batch.batch_info.clone().into_stored());
            manager.add_job(batch).await;
        }

        let sized_proof = FriProof::Real(RealFriProof {
            proof: Bytes::from(vec![0x5a; TEST_PROOF_BYTES]),
            proving_execution_version: ProvingVersion::V8 as u32,
        });
        let proof_wire_bytes = snark_pick_proof_wire_bytes(&sized_proof).unwrap();
        let response_limit = SNARK_PICK_FIXED_JSON_BUDGET + 2 * proof_wire_bytes;
        assert!(SNARK_PICK_FIXED_JSON_BUDGET + 3 * proof_wire_bytes > response_limit);
        let picked = manager
            .pick_real_job_with_response_limit(
                "capacity-split".to_owned(),
                Some(&[ProvingVersion::V8]),
                response_limit,
            )
            .await?
            .expect("response cap must release the safe prefix immediately");
        assert_eq!(picked.batches.len(), 2);
        assert_eq!(picked.batches[0].0.batch_number, 1);
        assert_eq!(picked.batches[1].0.batch_number, 2);
        let encoded_proof = general_purpose::STANDARD.encode(vec![0x5a; TEST_PROOF_BYTES]);
        let two_proof_payload = serde_json::json!({
            "from_batch_number": 1,
            "to_batch_number": 2,
            "vk_hash": v8_vk_hash(),
            "fri_proofs": [encoded_proof.clone(), encoded_proof.clone()],
            "lease_token": picked.lease_token,
        });
        assert!(serde_json::to_vec(&two_proof_payload)?.len() <= response_limit);
        let three_proof_payload = serde_json::json!({
            "from_batch_number": 1,
            "to_batch_number": 3,
            "vk_hash": v8_vk_hash(),
            "fri_proofs": [encoded_proof.clone(), encoded_proof.clone(), encoded_proof],
            "lease_token": "0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        });
        assert!(serde_json::to_vec(&three_proof_payload)?.len() > response_limit);
        Ok(())
    }

    // SYSCOIN: If the mandatory two-FRI response itself cannot fit, no lease is created and the
    // critical manager latches a deterministic operator-visible fault instead of polling forever.
    #[tokio::test]
    async fn unservable_two_proof_response_fails_before_leasing() -> anyhow::Result<()> {
        let (sender, _receiver) = mpsc::channel(1);
        let manager = SnarkJobManager::new(
            sender,
            100,
            100,
            Duration::from_secs(3600),
            Duration::from_secs(60),
            100,
        );
        let _ = add_two_contiguous_real_jobs(&manager).await;
        let proof_wire_bytes = snark_pick_proof_wire_bytes(&real_fri_proof()).unwrap();
        let response_limit = SNARK_PICK_FIXED_JSON_BUDGET + proof_wire_bytes;
        let error = manager
            .pick_real_job_with_response_limit(
                "capacity-fatal".to_owned(),
                Some(&[ProvingVersion::V8]),
                response_limit,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("response bytes"));
        assert!(manager.fatal_error.current().is_some());
        assert!(
            manager
                .status()
                .await
                .iter()
                .all(|job| job.assigned_to_prover_id.is_none())
        );
        Ok(())
    }

    #[tokio::test]
    async fn rehydrated_acceptance_age_releases_two_proof_real_range() -> anyhow::Result<()> {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let (sender, _receiver) = mpsc::channel(1);
        let manager = SnarkJobManager::new(
            sender,
            100,
            100,
            Duration::from_secs(3600),
            Duration::from_secs(60),
            100,
        );
        manager
            .add_rehydrated_job(
                create_test_batch_envelope_with_data(1, protocol_version.clone(), real_fri_proof()),
                Duration::from_secs(3601),
            )
            .await?;

        // SYSCOIN: The server's atomic pick is both the readiness decision and lease. Even after
        // the age threshold, a singleton must remain unassigned so a standalone CPU SNARK worker
        // cannot invent a local aggregation policy or duplicate speculative wrapping.
        assert!(
            manager
                .pick_real_job("cpu-snark-1".to_string(), Some(&[ProvingVersion::V8]))
                .await?
                .is_none()
        );
        assert_eq!(manager.status().await[0].assigned_to_prover_id, None);

        manager
            .add_job(create_test_batch_envelope_with_data(
                2,
                protocol_version,
                real_fri_proof(),
            ))
            .await;

        let picked = manager
            .pick_real_job("cpu-snark-1".to_string(), Some(&[ProvingVersion::V8]))
            .await?
            .expect("stored acceptance age must release a two-proof range after restart");
        assert_eq!(picked.len(), 2);
        assert_eq!(picked[0].0.batch_number, 1);
        assert_eq!(picked[1].0.batch_number, 2);
        Ok(())
    }

    // SYSCOIN: A restart may find one more canonical FRI than the bounded SNARK map can hold.
    // Recovery must fail immediately with an operator-actionable bound instead of waiting for the
    // still-closed prover listener to drain the queue.
    #[tokio::test]
    async fn rehydration_capacity_fails_instead_of_waiting_for_closed_api() -> anyhow::Result<()> {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let (sender, _receiver) = mpsc::channel(1);
        let manager = SnarkJobManager::new(
            sender,
            100,
            100,
            Duration::from_secs(3600),
            Duration::from_secs(60),
            2,
        );
        for batch_number in 1..=3 {
            manager
                .add_rehydrated_job(
                    create_test_batch_envelope_with_data(
                        batch_number,
                        protocol_version.clone(),
                        real_fri_proof(),
                    ),
                    Duration::ZERO,
                )
                .await?;
        }

        let error = tokio::time::timeout(
            Duration::from_millis(100),
            manager.add_rehydrated_job(
                create_test_batch_envelope_with_data(4, protocol_version, real_fri_proof()),
                Duration::ZERO,
            ),
        )
        .await
        .expect("rehydration must never await unavailable prover capacity")
        .expect_err("the fourth batch must exceed the configured recovery range");
        assert_eq!(error.batch_number, 4);
        assert_eq!((error.current_min, error.current_max), (1, 3));
        assert_eq!(error.max_assigned_batch_range, 2);
        assert_eq!(error.prover_stage, ProverStage::Snark);
        Ok(())
    }

    #[tokio::test]
    async fn l1_submission_markers_are_never_offered_to_real_snark_provers() -> anyhow::Result<()> {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let (sender, mut receiver) = mpsc::channel(1);
        let manager =
            SnarkJobManager::new(sender, 100, 2, Duration::ZERO, Duration::from_secs(60), 100);
        for batch_number in 1..=2 {
            manager
                .add_rehydrated_job(
                    create_test_batch_envelope_with_data(
                        batch_number,
                        protocol_version.clone(),
                        FriProof::AlreadySubmittedToL1,
                    ),
                    Duration::ZERO,
                )
                .await?;
        }

        // SYSCOIN: Before the explicit variant check, two markers met the aggregation floor and
        // were handed to a real wrapper despite containing no proof bytes.
        assert!(
            manager
                .pick_real_job("real-prover".into(), None)
                .await?
                .is_none()
        );
        assert!(receiver.try_recv().is_err());
        Ok(())
    }

    #[tokio::test]
    async fn rehydrated_interop_metadata_releases_fresh_two_proof_range() -> anyhow::Result<()> {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let (sender, _receiver) = mpsc::channel(1);
        let manager = SnarkJobManager::new(
            sender,
            100,
            100,
            Duration::from_secs(3600),
            Duration::from_secs(60),
            100,
        );
        manager
            .add_job(create_test_batch_envelope_with_data(
                1,
                protocol_version.clone(),
                real_fri_proof(),
            ))
            .await;
        let mut rehydrated_interop_batch =
            create_test_batch_envelope_with_data(2, protocol_version, real_fri_proof());
        mark_test_batch_as_interop_bundle(&mut rehydrated_interop_batch);
        manager
            .add_rehydrated_job(rehydrated_interop_batch, Duration::ZERO)
            .await?;

        let picked = manager
            .pick_real_job("snark-prover".to_string(), Some(&[ProvingVersion::V8]))
            .await?
            .expect("rehydrated interop metadata must retain its priority signal");
        assert_eq!(picked.len(), 2);
        assert_eq!(picked[0].0.batch_number, 1);
        assert_eq!(picked[1].0.batch_number, 2);
        Ok(())
    }

    #[tokio::test]
    async fn duplicate_after_rehydration_preserves_age_and_active_assignment() -> anyhow::Result<()>
    {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let (sender, mut receiver) = mpsc::channel(1);
        let manager = SnarkJobManager::new(
            sender,
            100,
            100,
            Duration::from_secs(1),
            Duration::from_secs(60),
            100,
        );
        manager
            .add_rehydrated_job(
                create_test_batch_envelope_with_data(1, protocol_version.clone(), real_fri_proof()),
                Duration::from_secs(2),
            )
            .await?;
        manager
            .add_job(create_test_batch_envelope_with_data(
                2,
                protocol_version.clone(),
                real_fri_proof(),
            ))
            .await;

        let assigned = manager
            .pick_real_job("snark-prover".to_string(), Some(&[ProvingVersion::V8]))
            .await?
            .expect("rehydrated age must make the two-proof range ready");
        assert_eq!(assigned.len(), 2);
        // SYSCOIN: Aggregate diagnostics must not reveal the bearer token.
        assert!(!format!("{assigned:?}").contains(&assigned.lease_token));

        // SYSCOIN: Knowing the public owner label and exact range is not authority.
        let spoofed = manager
            .submit_proof(
                1,
                2,
                v8_vk_hash(),
                encoded_snark_proof(ZKSYNC_OS_V8_REAL_PROOF_BYTES),
                "snark-prover".to_string(),
                "0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff".to_string(),
            )
            .await
            .unwrap_err();
        assert_eq!(spoofed.to_string(), "invalid or stale SNARK lease");
        assert_eq!(manager.status().await.len(), 2);

        // This is the normal recreated-pipeline arrival that follows startup rehydration.
        manager
            .add_job(create_test_batch_envelope_with_data(
                1,
                protocol_version,
                real_fri_proof(),
            ))
            .await;

        let status = manager.status().await;
        assert_eq!(status.len(), 2);
        assert!(status[0].added_seconds_ago >= 1);
        assert_eq!(
            status[0].assigned_to_prover_id.as_deref(),
            Some("snark-prover")
        );
        assert_eq!(status[0].current_attempt, 1);

        manager
            .submit_proof(
                1,
                2,
                v8_vk_hash(),
                encoded_snark_proof(ZKSYNC_OS_V8_REAL_PROOF_BYTES),
                // SYSCOIN: The bearer token, not this diagnostic ID, authorizes completion.
                "different-display-id".to_string(),
                assigned.lease_token.clone(),
            )
            .await?;
        assert!(receiver.recv().await.is_some());
        assert!(manager.status().await.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn reassigned_snark_range_rejects_old_token() -> anyhow::Result<()> {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let (sender, mut receiver) = mpsc::channel(1);
        let manager = SnarkJobManager::new(sender, 2, 2, Duration::ZERO, Duration::ZERO, 100);
        for batch_number in 1..=2 {
            manager
                .add_job(create_test_batch_envelope_with_data(
                    batch_number,
                    protocol_version.clone(),
                    real_fri_proof(),
                ))
                .await;
        }

        let first = manager
            .pick_real_job("display-a".to_string(), Some(&[ProvingVersion::V8]))
            .await?
            .expect("first range must be assigned");
        let second = manager
            .pick_real_job("display-b".to_string(), Some(&[ProvingVersion::V8]))
            .await?
            .expect("zero-timeout range must be reassigned");
        assert_ne!(first.lease_token, second.lease_token);

        let stale = manager
            .submit_proof(
                1,
                2,
                v8_vk_hash(),
                encoded_snark_proof(ZKSYNC_OS_V8_REAL_PROOF_BYTES),
                "display-a".to_string(),
                first.lease_token,
            )
            .await
            .unwrap_err();
        assert_eq!(stale.to_string(), "invalid or stale SNARK lease");
        assert_eq!(manager.status().await.len(), 2);

        manager
            .submit_proof(
                1,
                2,
                v8_vk_hash(),
                encoded_snark_proof(ZKSYNC_OS_V8_REAL_PROOF_BYTES),
                "display-c".to_string(),
                second.lease_token,
            )
            .await?;
        assert!(receiver.recv().await.is_some());
        assert!(manager.status().await.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn overlapping_snark_replay_waits_but_stale_reassignment_is_definitive()
    -> anyhow::Result<()> {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let (sender, _receiver) = mpsc::channel(2);
        let manager = SnarkJobManager::new(sender, 2, 2, Duration::ZERO, Duration::ZERO, 100);
        for batch_number in 1..=2 {
            manager
                .add_job(create_test_batch_envelope_with_data(
                    batch_number,
                    protocol_version.clone(),
                    real_fri_proof(),
                ))
                .await;
        }
        let picked = manager
            .pick_real_job("display-a".to_owned(), Some(&[ProvingVersion::V8]))
            .await?
            .expect("SNARK range must be assigned");
        let original = manager
            .jobs
            .begin_submission(1, 2, &picked.lease_token)
            .await
            .expect("original SNARK submission must hold the range");

        assert!(matches!(
            manager
                .submit_proof(
                    1,
                    2,
                    v8_vk_hash(),
                    encoded_snark_proof(ZKSYNC_OS_V8_REAL_PROOF_BYTES),
                    "display-b".to_owned(),
                    picked.lease_token.clone(),
                )
                .await,
            Err(SnarkSubmitError::SubmissionInProgress)
        ));
        original.release().await;
        let retry = manager
            .jobs
            .begin_submission(1, 2, &picked.lease_token)
            .await
            .expect("same aggregate token must be admissible after transient release");
        retry.release().await;

        let reassigned = manager
            .pick_real_job("display-c".to_owned(), Some(&[ProvingVersion::V8]))
            .await?
            .expect("zero-timeout range must be reassignable after release");
        assert_ne!(picked.lease_token, reassigned.lease_token);
        assert!(matches!(
            manager
                .submit_proof(
                    1,
                    2,
                    v8_vk_hash(),
                    encoded_snark_proof(ZKSYNC_OS_V8_REAL_PROOF_BYTES),
                    "display-a".to_owned(),
                    picked.lease_token,
                )
                .await,
            Err(SnarkSubmitError::InvalidLease)
        ));
        assert!(
            manager
                .status()
                .await
                .iter()
                .all(|job| job.assigned_to_prover_id.as_deref() == Some("display-c"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn only_exact_snark_token_decodes_and_malformed_owner_submission_revokes()
    -> anyhow::Result<()> {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let (sender, mut receiver) = mpsc::channel(1);
        let manager = SnarkJobManager::new(
            sender,
            2,
            2,
            Duration::from_secs(3600),
            Duration::from_secs(60),
            100,
        );
        manager
            .add_job(create_test_batch_envelope_with_data(
                1,
                protocol_version.clone(),
                real_fri_proof(),
            ))
            .await;
        manager
            .add_job(create_test_batch_envelope_with_data(
                2,
                protocol_version,
                real_fri_proof(),
            ))
            .await;
        let assigned = manager
            .pick_real_job("snark-prover".to_string(), Some(&[ProvingVersion::V8]))
            .await?
            .expect("target-sized range must be assigned");

        // SYSCOIN: Even a syntactically malformed body cannot reach decode work without the exact
        // capability. Public range/ID knowledge is not an allocation or CPU capability.
        let stale_err = manager
            .submit_proof(
                1,
                2,
                v8_vk_hash(),
                "not base64".to_owned(),
                "snark-prover".to_string(),
                "0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff".to_owned(),
            )
            .await
            .unwrap_err();
        assert!(matches!(stale_err, SnarkSubmitError::InvalidLease));
        assert_eq!(
            manager
                .proof_decode_invocations
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );

        let malformed_err = manager
            .submit_proof(
                1,
                2,
                v8_vk_hash(),
                "not base64".to_owned(),
                "snark-prover".to_string(),
                assigned.lease_token.clone(),
            )
            .await
            .unwrap_err();
        assert!(matches!(malformed_err, SnarkSubmitError::InvalidBase64(_)));
        assert_eq!(
            manager
                .proof_decode_invocations
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert!(
            manager
                .status()
                .await
                .iter()
                .all(|job| job.assigned_to_prover_id.is_none())
        );

        // Definitive malformed-owner rejection revokes immediately; no assignment timeout is
        // required before another wrapper can repick the exact range.
        let repicked = manager
            .pick_real_job(
                "replacement-wrapper".to_owned(),
                Some(&[ProvingVersion::V8]),
            )
            .await?
            .expect("revoked malformed submission must be immediately repickable");
        assert_ne!(assigned.lease_token, repicked.lease_token);
        manager
            .submit_proof(
                1,
                2,
                v8_vk_hash(),
                encoded_snark_proof(ZKSYNC_OS_V8_REAL_PROOF_BYTES),
                "different-diagnostic-id".to_string(),
                repicked.lease_token,
            )
            .await?;
        assert!(receiver.recv().await.is_some());
        assert!(manager.status().await.is_empty());
        Ok(())
    }

    // SYSCOIN: The pinned V32 verifier accepts 44 raw proof words. Even an aligned but shorter
    // authenticated payload is terminal before verifier RPC work and immediately releases the
    // aggregate for a different wrapper instead of pinning the head lease through retryable OOG.
    #[tokio::test]
    async fn wrong_sized_v8_wrapper_is_revoked_before_preflight() -> anyhow::Result<()> {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let (sender, _receiver) = mpsc::channel(1);
        let manager =
            SnarkJobManager::new(sender, 2, 2, Duration::ZERO, Duration::from_secs(60), 100);
        for batch_number in 1..=2 {
            manager
                .add_job(create_test_batch_envelope_with_data(
                    batch_number,
                    protocol_version.clone(),
                    real_fri_proof(),
                ))
                .await;
        }
        let assigned = manager
            .pick_real_job("wrong-shape".to_owned(), Some(&[ProvingVersion::V8]))
            .await?
            .expect("two-proof aggregate must be ready");

        let error = manager
            .submit_proof(
                1,
                2,
                v8_vk_hash(),
                encoded_snark_proof(32),
                "wrong-shape".to_owned(),
                assigned.lease_token.clone(),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, SnarkSubmitError::InvalidProofLength(32)));
        assert!(
            manager
                .status()
                .await
                .iter()
                .all(|job| job.assigned_to_prover_id.is_none())
        );

        let replacement = manager
            .pick_real_job("replacement".to_owned(), Some(&[ProvingVersion::V8]))
            .await?
            .expect("wrong-sized wrapper must release the range immediately");
        assert_ne!(replacement.lease_token, assigned.lease_token);
        Ok(())
    }

    #[tokio::test]
    async fn real_snark_backpressure_retains_exact_lease_for_retry() -> anyhow::Result<()> {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let (sender, mut receiver) = mpsc::channel(1);
        sender
            .try_send(ProofCommand::new(
                vec![create_test_batch_envelope_with_data(
                    100,
                    protocol_version.clone(),
                    FriProof::Fake,
                )],
                SnarkProof::Fake,
            ))
            .unwrap();
        let manager =
            SnarkJobManager::new(sender, 2, 2, Duration::from_secs(3600), Duration::ZERO, 100);
        for batch_number in 1..=2 {
            manager
                .add_job(create_test_batch_envelope_with_data(
                    batch_number,
                    protocol_version.clone(),
                    real_fri_proof(),
                ))
                .await;
        }
        let assigned = manager
            .pick_real_job("real-wrapper".to_owned(), Some(&[ProvingVersion::V8]))
            .await?
            .expect("real aggregate must be leased");

        assert!(matches!(
            manager
                .submit_proof(
                    1,
                    2,
                    v8_vk_hash(),
                    encoded_snark_proof(ZKSYNC_OS_V8_REAL_PROOF_BYTES),
                    "different-display".to_owned(),
                    assigned.lease_token.clone(),
                )
                .await,
            Err(SnarkSubmitError::DownstreamBackpressure)
        ));
        // SYSCOIN: The exact token is admitted, but transient capacity is checked before decoding;
        // retrying the same expensive proof under 429 does not repeat a 10 MiB allocation.
        assert_eq!(
            manager
                .proof_decode_invocations
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert!(
            manager
                .status()
                .await
                .iter()
                .all(|job| { job.assigned_to_prover_id.as_deref() == Some("real-wrapper") })
        );

        receiver.recv().await.expect("release downstream capacity");
        manager
            .submit_proof(
                1,
                2,
                v8_vk_hash(),
                encoded_snark_proof(ZKSYNC_OS_V8_REAL_PROOF_BYTES),
                "different-display".to_owned(),
                assigned.lease_token,
            )
            .await?;
        assert!(receiver.recv().await.is_some());
        assert!(manager.status().await.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn external_submit_must_match_exact_assigned_range() -> anyhow::Result<()> {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let (sender, mut receiver) = mpsc::channel(1);
        let manager = SnarkJobManager::new(
            sender,
            100,
            2,
            Duration::from_secs(3600),
            Duration::from_secs(60),
            100,
        );
        for batch_number in 1..=2 {
            manager
                .add_job(create_test_batch_envelope_with_data(
                    batch_number,
                    protocol_version.clone(),
                    real_fri_proof(),
                ))
                .await;
        }

        let assigned = manager
            .pick_real_job("snark-prover".to_string(), Some(&[ProvingVersion::V8]))
            .await?
            .expect("target-sized range must be assigned");
        assert_eq!(assigned.len(), 2);

        let err = manager
            .submit_proof(
                1,
                1,
                v8_vk_hash(),
                encoded_snark_proof(ZKSYNC_OS_V8_REAL_PROOF_BYTES),
                "snark-prover".to_string(),
                assigned.lease_token.clone(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "invalid or stale SNARK lease");
        assert_eq!(manager.status().await.len(), 2);

        manager
            .submit_proof(
                1,
                2,
                v8_vk_hash(),
                encoded_snark_proof(ZKSYNC_OS_V8_REAL_PROOF_BYTES),
                "snark-prover".to_string(),
                assigned.lease_token.clone(),
            )
            .await?;
        let command = receiver
            .recv()
            .await
            .expect("valid proof must be forwarded");
        assert_eq!(command.as_ref().len(), 2);
        assert!(manager.status().await.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn stale_fake_snark_aggregate_cannot_consume_reassigned_real_lease() -> anyhow::Result<()>
    {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let (sender, _receiver) = mpsc::channel(1);
        let manager = SnarkJobManager::new(sender, 2, 2, Duration::ZERO, Duration::ZERO, 100);
        for batch_number in 1..=2 {
            manager
                .add_job(create_test_batch_envelope_with_data(
                    batch_number,
                    protocol_version.clone(),
                    real_fri_proof(),
                ))
                .await;
        }

        let fake = manager
            .jobs
            .pick_leased_jobs_while_with_limit(2, "fake_prover", |_| true)
            .await
            .expect("fake scheduler must first lease the aggregate");
        let real = manager
            .pick_real_job("real-wrapper".to_owned(), Some(&[ProvingVersion::V8]))
            .await?
            .expect("zero-timeout aggregate must be reassigned to the real wrapper");
        assert_ne!(fake.lease_token, real.lease_token);

        assert!(
            manager
                .complete_fake_leased_jobs(1, 2, "fake_prover", &fake.lease_token)
                .await
                .is_none()
        );
        let status = manager.status().await;
        assert!(
            status
                .iter()
                .all(|job| { job.assigned_to_prover_id.as_deref() == Some("real-wrapper") })
        );
        let real_submission = manager
            .jobs
            .begin_submission(1, 2, &real.lease_token)
            .await
            .expect("stale fake aggregate must preserve the fresh real lease");
        real_submission.release().await;
        Ok(())
    }

    #[tokio::test]
    async fn backpressure_does_not_lease_fake_jobs() {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let (sender, mut receiver) = mpsc::channel(1);
        sender
            .try_send(ProofCommand::new(
                vec![create_test_batch_envelope_with_data(
                    100,
                    protocol_version.clone(),
                    FriProof::Fake,
                )],
                SnarkProof::Fake,
            ))
            .unwrap();

        let manager = SnarkJobManager::new(
            sender,
            2,
            2,
            Duration::from_secs(3600),
            Duration::from_secs(60),
            100,
        );
        let fake_input = create_test_batch_envelope_with_data(1, protocol_version, FriProof::Fake);
        let fake_metadata = fake_input.batch.clone();
        assert_eq!(
            manager.add_job(fake_input).await,
            SnarkJobAdmission::Inserted
        );

        let err = manager.process_pending_fake_fri_proofs().await.unwrap_err();
        assert_eq!(err.to_string(), "downstream backpressure");
        let status = manager.jobs.status().await;
        assert_eq!(status[0].assigned_to_prover_id, None);
        assert_eq!(status[0].current_attempt, 0);

        receiver.recv().await.unwrap();
        manager.process_pending_fake_fri_proofs().await.unwrap();

        let command = receiver.recv().await.unwrap();
        assert_eq!(command.as_ref()[0].batch_number(), 1);
        assert!(manager.jobs.status().await.is_empty());
        assert_eq!(
            manager
                .add_job(job_from_metadata(&fake_metadata, FriProof::Fake))
                .await,
            SnarkJobAdmission::AlreadyOwned
        );
        manager.process_pending_fake_fri_proofs().await.unwrap();
        assert!(manager.jobs.status().await.is_empty());
        assert!(
            receiver.try_recv().is_err(),
            "fake wrapper dispatched twice"
        );
    }
}
