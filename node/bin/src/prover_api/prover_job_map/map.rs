use super::models::{
    JobBatchStats, JobEntry, JobMetadata, NonEmptyQueueStatistics, ProverLeaseToken,
    QueueStatistics, canonical_batch_metadata_digest,
};
use super::recovery_boundary::{
    SnarkRecoveryBoundary, StartupRecoveryBoundaryError, StartupRecoveryPhase,
};
use super::tracked_lock::TrackedLockGuard;
use super::{PlannedSnarkRange, StartupRecoveryPlan, completed_ownership::SnarkCompletedOwnership};
use crate::prover_api::fri_job_manager::{FriJob, JobState};
use crate::prover_api::metrics::{JobMapMethod, PROVER_METRICS, ProverStage, ProverType};
use crate::prover_api::snark_proof_journal::{
    MAX_JOURNAL_RECORD_BYTES, durable_snark_batch_json_bytes, durable_snark_record_json_upper_bound,
};
use alloy::primitives::B256;
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Notify};
use zksync_os_batch_types::batcher_model::{
    BatchMetadata, BatchSignatureData, SignedBatchEnvelope,
};
use zksync_os_types::ProvingVersion;

// SYSCOIN: Never use attacker-selectable diagnostic prover IDs as Prometheus label values.
const CAPABILITY_AUTHENTICATED_METRICS_ID: &str = "capability-authenticated";

/// Concurrent map of prover jobs that support FRI and SNARK workflows.
/// Imposes a limit on batch range
/// Keys are batch numbers stored in a BTreeMap for ordered iteration.
/// Values are prover input - concrete types depend on the prover stage
///     (FRI - prover_input (Vec<u32>), SNARK - fri_proof).
///  * add_job - adds a new job (one batch)
///     * blocks if adding this job would exceed max_assigned_batch_range until space is available
///  * pick_job - picks the first job that is either pending or assigned and older than min_age
///     * currently, it iterates over all jobs and picks the first one that meets the criteria
///  * SubmissionLease completion - removes only the exact token-authenticated assignment
///
/// Current implementation uses async Mutex which is locked on each operation -
///     that is, prover requests to polling/submitting are sequential only.
/// This works for ~100s-1000s of jobs.
/// If needed, can be augmented by pointers to the oldest job and the first unpicked job -
/// this way polling is O(log n) not O(n).
///
/// This works both for FRI and SNARK jobs by allowing to pick multiple jobs atomically.
/// We don't maintain the SNARK job grouping - so that on timeout, a different range may be assigned instead.
///
#[derive(Debug)]
pub struct ProverJobMap<T> {
    // == state ==
    // SYSCOIN: SNARK admission and completion take this before `jobs`, making a durable/command
    // ownership claim atomic with removal while never retaining either lock across capacity waits.
    completed_ownership: Mutex<SnarkCompletedOwnership>,
    // SYSCOIN: Startup ordering sits between completed ownership and live jobs in the global lock
    // order, preventing a later/live aggregate from jumping an incompletely loaded recovery head.
    startup_recovery: Mutex<SnarkRecoveryBoundary>,
    // SYSCOIN: A completed FRI remains an endpoint-capacity fence until its durable proof is
    // accepted downstream or the exact handoff rolls back. This lock is always taken before jobs.
    fri_rollback_reservations: Mutex<BTreeMap<u64, FriRollbackReservationRecord>>,
    jobs: Mutex<BTreeMap<u64, JobEntry<T>>>,
    // Notification for waiting when batch range limit is hit (`max_assigned_batch_range`)
    space_available: Notify,

    // == config ==
    // assigns to another prover if it takes longer than this
    assignment_timeout: Duration,
    // maximum allowed range between min and max batch numbers
    max_assigned_batch_range: usize,
    // FRI/SNARK - used in logging
    prover_stage: ProverStage,
}

// SYSCOIN: Admission excludes same-batch replay while a durable handoff owns this exact opaque
// record, so there can be only one rollback disposition for each batch.
#[derive(Debug)]
struct FriRollbackReservationRecord {
    token: ProverLeaseToken,
    batch_metadata_digest: B256,
}

// SYSCOIN: Consume only one exact internal rollback capability. A stale task can never release a
// newer reservation for the same batch endpoint.
fn take_fri_rollback_reservation(
    reservations: &mut BTreeMap<u64, FriRollbackReservationRecord>,
    batch_number: u64,
    token: &ProverLeaseToken,
) {
    let record = reservations
        .get(&batch_number)
        .expect("reserved FRI batch disappeared before terminal disposition");
    assert_eq!(
        &record.token, token,
        "exact FRI rollback reservation changed before terminal disposition"
    );
    reservations
        .remove(&batch_number)
        .expect("validated FRI rollback reservation must remain present");
}

/// SYSCOIN: A SNARK FRI input is either newly queued, an idempotent replay of a live entry, or
/// permanently excluded because an already-published wrapper owns it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnarkJobAdmission {
    Inserted,
    Duplicate,
    AlreadyOwned,
}

/// SYSCOIN: Exact-token completion distinguishes a stale capability from a range already handed
/// to durable recovery or the downstream command pipeline.
pub enum SnarkOwnershipCompletion<T> {
    Completed(Vec<SignedBatchEnvelope<T>>),
    AlreadyOwned,
    Stale,
}

/// SYSCOIN: Canonically validated recovery may seed ownership only before any overlapping live
/// queue entry exists; an overlap means startup ordering or recovery validation regressed.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SnarkOwnershipSeedError {
    #[error("invalid recovered SNARK ownership range {from}-{to}")]
    InvalidRange { from: u64, to: u64 },
    #[error(
        "recovered SNARK ownership range {from}-{to} overlaps live queued batch {batch_number}"
    )]
    ActiveJob {
        from: u64,
        to: u64,
        batch_number: u64,
    },
}

/// SYSCOIN: Startup recovery cannot wait for a prover to drain a listener that is intentionally
/// still closed. Return the exact bounded-map state so the owning pipeline fails explicitly.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error(
    "batch {batch_number} cannot enter the full {prover_stage:?} job map: current range {current_min}-{current_max}, max_assigned_batch_range={max_assigned_batch_range}"
)]
pub struct JobMapCapacityExceeded {
    pub batch_number: u64,
    pub current_min: u64,
    pub current_max: u64,
    pub max_assigned_batch_range: usize,
    pub prover_stage: ProverStage,
}

/// SYSCOIN: Diagnostic state for an aggregate held below its target-or-age threshold.
#[derive(Debug)]
pub struct SnarkReadinessWait {
    pub eligible_fris: usize,
    pub oldest_eligible_age: Duration,
}

/// SYSCOIN: Distinguish a topology/version boundary from a hard response-byte boundary. Collapsing
/// both to `false` makes a permanently capped aggregate wait for target/age instead of releasing.
pub enum SnarkJobEligibility {
    Eligible,
    Incompatible,
    ResponseCapacityExceeded {
        required_bytes: usize,
        max_bytes: usize,
    },
}

/// SYSCOIN: Atomic outcome of readiness inspection and aggregate leasing.
pub enum SnarkJobPick<T> {
    Assigned {
        jobs: Vec<(FriJob, T)>,
        lease_token: String,
    },
    Waiting(SnarkReadinessWait),
    // SYSCOIN: The oldest contiguous two-FRI aggregate can never fit the durable record cap.
    // No capability was created; continuing to poll would otherwise wedge this queue forever.
    Unpersistable {
        batch_from: u64,
        blocked_at: u64,
        required_bytes: usize,
        max_bytes: usize,
    },
    // SYSCOIN: Even the oldest two-proof response cannot cross the configured HTTP byte boundary.
    // No capability was created; the critical pipeline must stop instead of polling forever.
    UnservableResponse {
        batch_from: u64,
        blocked_at: u64,
        required_bytes: usize,
        max_bytes: usize,
    },
    // SYSCOIN: A byte-limited startup prefix would strand an interior real singleton. No lease
    // exists, so the critical manager can fail explicitly instead of jumping later work.
    Unwrappable {
        batch_from: u64,
        batch_to: u64,
        fittable_fris: usize,
    },
    Empty,
}

// SYSCOIN: Custom debug output must never expose the live aggregate lease capability.
impl<T> Debug for SnarkJobPick<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Assigned { jobs, .. } => f
                .debug_struct("SnarkJobPick::Assigned")
                .field("job_count", &jobs.len())
                .field("lease_token", &"[REDACTED]")
                .finish(),
            Self::Waiting(wait) => f.debug_tuple("SnarkJobPick::Waiting").field(wait).finish(),
            Self::Unpersistable {
                batch_from,
                blocked_at,
                required_bytes,
                max_bytes,
            } => f
                .debug_struct("SnarkJobPick::Unpersistable")
                .field("batch_from", batch_from)
                .field("blocked_at", blocked_at)
                .field("required_bytes", required_bytes)
                .field("max_bytes", max_bytes)
                .finish(),
            Self::UnservableResponse {
                batch_from,
                blocked_at,
                required_bytes,
                max_bytes,
            } => f
                .debug_struct("SnarkJobPick::UnservableResponse")
                .field("batch_from", batch_from)
                .field("blocked_at", blocked_at)
                .field("required_bytes", required_bytes)
                .field("max_bytes", max_bytes)
                .finish(),
            Self::Unwrappable {
                batch_from,
                batch_to,
                fittable_fris,
            } => f
                .debug_struct("SnarkJobPick::Unwrappable")
                .field("batch_from", batch_from)
                .field("batch_to", batch_to)
                .field("fittable_fris", fittable_fris)
                .finish(),
            Self::Empty => f.write_str("SnarkJobPick::Empty"),
        }
    }
}

/// SYSCOIN: A picked FRI job and the opaque capability required to submit it.
pub struct LeasedJob<T> {
    pub job: FriJob,
    pub data: T,
    pub lease_token: String,
}

impl<T> Debug for LeasedJob<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeasedJob")
            .field("job", &self.job)
            .field("data", &"[OMITTED]")
            .field("lease_token", &"[REDACTED]")
            .finish()
    }
}

/// SYSCOIN: A picked internal aggregate and the exact capability needed to consume it later.
pub(crate) struct LeasedJobs<T> {
    pub jobs: Vec<(FriJob, T)>,
    pub lease_token: String,
}

impl<T> Debug for LeasedJobs<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeasedJobs")
            .field("job_count", &self.jobs.len())
            .field("lease_token", &"[REDACTED]")
            .finish()
    }
}

/// SYSCOIN: Why an external submission could not acquire its exact current lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BeginSubmissionError {
    InvalidRange,
    UnknownJob,
    InvalidLease,
    AlreadySubmitting,
}

/// SYSCOIN: RAII ownership of one exact external submission admitted by the job map.
///
/// Dropping the request-side future cannot strand `submission_in_progress`: the cleanup compares
/// the opaque token again, so it can never clear a newer reassignment.
pub struct SubmissionLease<T: Clone + Send + 'static> {
    jobs: Arc<ProverJobMap<T>>,
    batch_range: (u64, u64),
    token: ProverLeaseToken,
    batch_snapshots: Vec<SubmissionBatchSnapshot>,
    active: bool,
}

/// SYSCOIN: Owns both a removed FRI job and its exact endpoint-capacity reservation until the
/// durable handoff commits or rolls back. Dropping this guard schedules a lossless rollback.
#[must_use = "a reserved FRI job must be committed or rolled back"]
pub(crate) struct ReservedFriJob<T: Clone + Send + 'static> {
    jobs: Arc<ProverJobMap<T>>,
    batch_number: u64,
    token: Option<ProverLeaseToken>,
    batch_metadata_digest: B256,
    batch_envelope: Option<SignedBatchEnvelope<T>>,
    active: bool,
}

// SYSCOIN: Snapshot only immutable verifier inputs, never the potentially multi-megabyte prover
// input/proof payload or the non-cloneable pipeline latency tracker.
struct SubmissionBatchSnapshot {
    proving_version: ProvingVersion,
    // SYSCOIN: Carry the already-computed authoritative identity into the atomic rollback fence;
    // never reserialize attacker-sized batch metadata while the reservation lock is held.
    batch_metadata_digest: B256,
    // SYSCOIN: Only the single-batch FRI verifier needs full logs/messages/signatures. A SNARK
    // aggregate snapshots its immutable version per batch without cloning up to 100 large batches.
    fri_batch: Option<BatchMetadata>,
    fri_signature_data: Option<BatchSignatureData>,
}

// SYSCOIN: Never expose the internal rollback capability through logs or assertion output.
impl<T: Clone + Send + 'static> Debug for ReservedFriJob<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReservedFriJob")
            .field("batch_number", &self.batch_number)
            .field("reservation_token", &"[REDACTED]")
            .field("active", &self.active)
            .finish()
    }
}

impl<T: Clone + Send + 'static> ReservedFriJob<T> {
    /// SYSCOIN: Apply the pipeline-stage transition while retaining rollback ownership of the
    /// original prover input and its endpoint-capacity fence.
    pub(crate) fn map_batch_envelope(
        mut self,
        map: impl FnOnce(SignedBatchEnvelope<T>) -> SignedBatchEnvelope<T>,
    ) -> Self {
        let batch_envelope = self
            .batch_envelope
            .take()
            .expect("active reserved FRI job must own its batch envelope");
        self.batch_envelope = Some(map(batch_envelope));
        self
    }

    /// SYSCOIN: The accepted-proof forwarder moves only latency state; the guarded prover input
    /// remains available for exact rollback until downstream ownership is confirmed.
    pub(crate) fn batch_envelope_mut(&mut self) -> &mut SignedBatchEnvelope<T> {
        self.batch_envelope
            .as_mut()
            .expect("active reserved FRI job must own its batch envelope")
    }

    /// SYSCOIN: A pending proof reloaded from disk must still describe the exact canonical batch
    /// whose live queue entry this guard removed; a swapped file is quarantined before handoff.
    pub(crate) fn matches_batch_metadata<U>(
        &self,
        batch_envelope: &SignedBatchEnvelope<U>,
    ) -> bool {
        self.batch_number == batch_envelope.batch_number()
            && self.batch_metadata_digest == canonical_batch_metadata_digest(batch_envelope)
    }

    /// SYSCOIN: Downstream acceptance is terminal. Run reservation release in an owned task so
    /// cancellation after a successful channel send cannot leave an in-memory capacity fence.
    pub(crate) async fn commit(mut self) {
        let jobs = Arc::clone(&self.jobs);
        let batch_number = self.batch_number;
        let token = self
            .token
            .take()
            .expect("active reserved FRI job must own its rollback token");
        self.batch_envelope.take();
        self.active = false;
        tokio::spawn(async move {
            jobs.release_fri_rollback_reservation(batch_number, &token)
                .await;
        })
        .await
        .expect("FRI rollback-reservation release task panicked");
    }

    /// SYSCOIN: Failed handoff atomically swaps this already-counted endpoint reservation back to
    /// a live job. The owned task survives cancellation of the forwarder branch that initiated it.
    pub(crate) async fn rollback(mut self) {
        let jobs = Arc::clone(&self.jobs);
        let batch_number = self.batch_number;
        let token = self
            .token
            .take()
            .expect("active reserved FRI job must own its rollback token");
        let batch_envelope = self
            .batch_envelope
            .take()
            .expect("active reserved FRI job must own its batch envelope");
        self.active = false;
        tokio::spawn(async move {
            jobs.restore_reserved_fri_job(batch_number, &token, batch_envelope)
                .await;
        })
        .await
        .expect("FRI reserved-job rollback task panicked");
    }
}

// SYSCOIN: If an accepted-proof message or its critical forwarder is dropped while the runtime is
// still alive, restore the exact job. Full runtime termination falls back to durable proof recovery.
impl<T: Clone + Send + 'static> Drop for ReservedFriJob<T> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let Some(token) = self.token.take() else {
            return;
        };
        let Some(batch_envelope) = self.batch_envelope.take() else {
            return;
        };
        let jobs = Arc::clone(&self.jobs);
        let batch_number = self.batch_number;
        self.active = false;
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                jobs.restore_reserved_fri_job(batch_number, &token, batch_envelope)
                    .await;
            });
        }
    }
}

impl<T: Clone + Send + 'static> Debug for SubmissionLease<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubmissionLease")
            .field("batch_range", &self.batch_range)
            .field("lease_token", &"[REDACTED]")
            .field("batch_count", &self.batch_snapshots.len())
            .field("active", &self.active)
            .finish()
    }
}

impl<T: Clone + Send + 'static> SubmissionLease<T> {
    /// SYSCOIN: Read every immutable metadata snapshot admitted under this token without rereading
    /// mutable job-map state or cloning the queued prover payload.
    pub fn batch_metadata(&self) -> impl Iterator<Item = &BatchMetadata> {
        self.batch_snapshots
            .iter()
            .filter_map(|snapshot| snapshot.fri_batch.as_ref())
    }

    /// SYSCOIN: SNARK verification/preflight needs only the immutable version selected at pick.
    pub fn proving_versions(&self) -> impl ExactSizeIterator<Item = ProvingVersion> + '_ {
        self.batch_snapshots
            .iter()
            .map(|snapshot| snapshot.proving_version)
    }

    pub fn first_batch_metadata(&self) -> Option<&BatchMetadata> {
        self.batch_metadata().next()
    }

    pub fn first_signature_data(&self) -> Option<&BatchSignatureData> {
        self.batch_snapshots
            .first()
            .and_then(|snapshot| snapshot.fri_signature_data.as_ref())
    }

    /// SYSCOIN: Snapshot canonical metadata for a durable SNARK handoff only after the wrapper and
    /// exact capability have been validated. This excludes both queued multi-megabyte FRI payloads
    /// and already-consumed commit signatures; the journal records an `AlreadyCommitted` marker.
    pub async fn durable_snark_batches(&self) -> Option<Vec<BatchMetadata>> {
        if self.jobs.prover_stage != ProverStage::Snark {
            return None;
        }
        let jobs = self
            .jobs
            .lock_with_tracking(JobMapMethod::GetJobBatchMetadata)
            .await;
        let mut snapshots = Vec::with_capacity(self.batch_snapshots.len());
        for batch_number in self.batch_range.0..=self.batch_range.1 {
            let entry = jobs.get(&batch_number)?;
            if entry.metadata.assigned_batch_range != Some(self.batch_range)
                || entry.metadata.assigned_lease_token.as_ref() != Some(&self.token)
                || !entry.metadata.submission_in_progress
            {
                return None;
            }
            snapshots.push(entry.batch_envelope.batch.clone());
        }
        Some(snapshots)
    }

    pub(crate) async fn complete_fake_fri(
        mut self,
        prover_id: &str,
    ) -> Option<Vec<SignedBatchEnvelope<T>>> {
        // SYSCOIN: Only fake FRI owns the pre-reserved downstream fast path. Real FRI completion
        // must retain rollback capacity through `complete_fri_with_rollback_reservation`.
        assert_eq!(self.jobs.prover_stage, ProverStage::Fri);
        let completed = self
            .jobs
            .complete_leased_many_jobs(
                self.batch_range.0,
                self.batch_range.1,
                ProverType::Fake,
                prover_id,
                &self.token,
            )
            .await;
        if completed.is_some() {
            self.active = false;
        }
        completed
    }

    /// SYSCOIN: A real FRI completion atomically replaces its live queue endpoint with an exact
    /// rollback reservation. Admission therefore cannot advance past space that a delayed handoff
    /// may still need, while fake FRI completion retains its pre-reserved downstream fast path.
    pub(crate) async fn complete_fri_with_rollback_reservation(
        mut self,
        prover_id: &str,
    ) -> Option<ReservedFriJob<T>> {
        assert_eq!(self.jobs.prover_stage, ProverStage::Fri);
        assert_eq!(self.batch_range.0, self.batch_range.1);
        let batch_number = self.batch_range.0;
        let batch_metadata_digest = self
            .batch_snapshots
            .first()
            .expect("single FRI submission must contain one metadata snapshot")
            .batch_metadata_digest;
        let reservation_token = ProverLeaseToken::generate();

        // SYSCOIN: This mutex is both the reservation store and the admission gate. Holding it
        // across exact completion closes the remove/notify/admit race without rewriting completion.
        let mut reservations = self.jobs.fri_rollback_reservations.lock().await;
        assert!(
            !reservations.contains_key(&batch_number),
            "FRI batch acquired overlapping rollback reservations"
        );
        let mut completed = self
            .jobs
            .complete_leased_many_jobs(
                batch_number,
                batch_number,
                ProverType::Real,
                prover_id,
                &self.token,
            )
            .await?;
        let batch_envelope = completed
            .pop()
            .expect("single FRI completion must remove one job");
        debug_assert!(completed.is_empty());
        let replaced = reservations.insert(
            batch_number,
            FriRollbackReservationRecord {
                token: reservation_token.clone(),
                batch_metadata_digest,
            },
        );
        debug_assert!(replaced.is_none());
        self.active = false;
        drop(reservations);

        Some(ReservedFriJob {
            jobs: Arc::clone(&self.jobs),
            batch_number,
            token: Some(reservation_token),
            batch_metadata_digest,
            batch_envelope: Some(batch_envelope),
            active: true,
        })
    }

    /// SYSCOIN: Claim completed SNARK ownership and remove the exact leased range under one lock
    /// order. `AlreadyOwned` is terminal for this lease; `Stale` leaves RAII cleanup active.
    pub async fn complete_with_snark_ownership(
        mut self,
        prover_type: ProverType,
        prover_id: &str,
    ) -> SnarkOwnershipCompletion<T> {
        let completed = self
            .jobs
            .complete_leased_many_jobs_with_ownership(
                self.batch_range.0,
                self.batch_range.1,
                prover_type,
                prover_id,
                &self.token,
            )
            .await;
        if matches!(
            completed,
            SnarkOwnershipCompletion::Completed(_) | SnarkOwnershipCompletion::AlreadyOwned
        ) {
            self.active = false;
        }
        completed
    }

    pub async fn revoke(mut self) {
        self.jobs
            .revoke_submission(self.batch_range, &self.token)
            .await;
        self.active = false;
    }

    pub async fn release(mut self) {
        self.jobs
            .release_submission(self.batch_range, &self.token)
            .await;
        self.active = false;
    }

    /// SYSCOIN: A validated expensive-proof retry refreshes its assignment clock while releasing
    /// only the in-progress guard. This prevents a disk-full/backpressure response from making the
    /// exact wrapper capability immediately stealable under its original, hours-old pick time.
    pub async fn release_for_retry(mut self) {
        self.jobs
            .release_submission_for_retry(self.batch_range, &self.token)
            .await;
        self.active = false;
    }
}

impl<T: Clone + Send + 'static> Drop for SubmissionLease<T> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let jobs = Arc::clone(&self.jobs);
        let batch_range = self.batch_range;
        let token = self.token.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                jobs.release_submission(batch_range, &token).await;
            });
        }
    }
}

impl<T: Clone> ProverJobMap<T> {
    pub fn new(
        assignment_timeout: Duration,
        max_assigned_batch_range: usize,
        prover_stage: ProverStage,
    ) -> Self {
        Self {
            completed_ownership: Mutex::new(SnarkCompletedOwnership::default()),
            startup_recovery: Mutex::new(SnarkRecoveryBoundary::default()),
            // SYSCOIN: No endpoint rollback is pending when a stage-local job map is created.
            fri_rollback_reservations: Mutex::new(BTreeMap::new()),
            jobs: Mutex::new(BTreeMap::new()),
            space_available: Notify::new(),
            assignment_timeout,
            max_assigned_batch_range,
            prover_stage,
        }
    }

    #[cfg(test)]
    pub(crate) async fn lock_jobs_for_test(
        &self,
    ) -> tokio::sync::MutexGuard<'_, BTreeMap<u64, JobEntry<T>>> {
        self.jobs.lock().await
    }

    /// SYSCOIN: Seed only canonically validated durable-journal ranges before the SNARK pipeline
    /// becomes drainable. Validation and all coalescing claims are atomic with live admission.
    pub async fn seed_snark_completed_ownership(
        &self,
        recovered_ranges: &[(u64, u64)],
    ) -> Result<(), SnarkOwnershipSeedError> {
        assert_eq!(self.prover_stage, ProverStage::Snark);
        for &(from, to) in recovered_ranges {
            if from > to {
                return Err(SnarkOwnershipSeedError::InvalidRange { from, to });
            }
        }

        let mut ownership = self.completed_ownership.lock().await;
        let _boundary = self.startup_recovery.lock().await;
        let jobs = self.lock_with_tracking(JobMapMethod::AddJob).await;
        for &(from, to) in recovered_ranges {
            if let Some((&batch_number, _)) = jobs.range(from..=to).next() {
                return Err(SnarkOwnershipSeedError::ActiveJob {
                    from,
                    to,
                    batch_number,
                });
            }
        }
        for &(from, to) in recovered_ranges {
            ownership.claim(from, to);
        }
        Ok(())
    }

    /// SYSCOIN: Install the immutable numeric recovery order before the prover surface becomes
    /// Drainable. A preexisting live job or completed overlap means startup ordering is too late.
    pub async fn install_startup_recovery_plan(
        &self,
        plan: StartupRecoveryPlan,
    ) -> Result<(), StartupRecoveryBoundaryError> {
        assert_eq!(self.prover_stage, ProverStage::Snark);
        let ownership = self.completed_ownership.lock().await;
        let mut boundary = self.startup_recovery.lock().await;
        let jobs = self.lock_with_tracking(JobMapMethod::AddJob).await;
        if let Some((&batch_number, _)) = jobs.first_key_value() {
            return Err(StartupRecoveryBoundaryError::ActiveJob(batch_number));
        }
        for range in plan.ranges() {
            if let Some(batch_number) =
                ownership.first_overlap(range.batch_from(), range.batch_to())
            {
                return Err(StartupRecoveryBoundaryError::AlreadyOwned(batch_number));
            }
        }
        if let Some(deferred_tip) = plan.deferred_tip()
            && let Some(batch_number) = ownership.first_overlap(deferred_tip, deferred_tip)
        {
            return Err(StartupRecoveryBoundaryError::AlreadyOwned(batch_number));
        }
        boundary.install(plan)
    }

    /// SYSCOIN: Loading may finish while provers still own planned heads. Keep the boundary in
    /// Draining until exact ownership completion consumes every planned range.
    pub async fn finish_startup_loading(&self) -> Result<(), StartupRecoveryBoundaryError> {
        assert_eq!(self.prover_stage, ProverStage::Snark);
        self.startup_recovery.lock().await.finish_loading()
    }

    #[cfg(test)]
    async fn completed_ownership_ranges_for_test(&self) -> Vec<(u64, u64)> {
        self.completed_ownership.lock().await.ranges()
    }

    /// Adds a pending job to the map.
    /// Awaits if adding this job exceeds `max_assigned_batch_range` until space is available.
    pub async fn add_job(&self, batch_envelope: SignedBatchEnvelope<T>) {
        self.add_job_with_age(batch_envelope, Duration::ZERO).await;
    }

    /// SYSCOIN: Production SNARK admission exposes completed ownership instead of silently
    /// treating a replayed FRI as newly available wrapping work.
    pub async fn admit_snark_job(
        &self,
        batch_envelope: SignedBatchEnvelope<T>,
    ) -> SnarkJobAdmission {
        assert_eq!(self.prover_stage, ProverStage::Snark);
        self.add_job_with_age_inner(batch_envelope, Duration::ZERO, true)
            .await
            .expect("blocking prover-job admission cannot return a capacity error")
    }

    /// SYSCOIN: Startup recovery preserves durable acceptance age while using live backpressure;
    /// the Drainable prover listener can consume exact planned heads to release bounded capacity.
    pub async fn admit_snark_job_with_age(
        &self,
        batch_envelope: SignedBatchEnvelope<T>,
        existing_age: Duration,
    ) -> SnarkJobAdmission {
        assert_eq!(self.prover_stage, ProverStage::Snark);
        self.add_job_with_age_inner(batch_envelope, existing_age, true)
            .await
            .expect("blocking prover-job admission cannot return a capacity error")
    }

    /// SYSCOIN: Adds a pending job while preserving age reconstructed from durable storage.
    pub async fn add_job_with_age(
        &self,
        batch_envelope: SignedBatchEnvelope<T>,
        existing_age: Duration,
    ) {
        let _ = self
            .add_job_with_age_inner(batch_envelope, existing_age, true)
            .await
            .expect("blocking prover-job admission cannot return a capacity error");
    }

    /// SYSCOIN: Rehydrate only when capacity is immediately available. Waiting here would
    /// deadlock production startup because the external prover listener opens after recovery.
    #[cfg(test)]
    pub async fn try_add_job_with_age(
        &self,
        batch_envelope: SignedBatchEnvelope<T>,
        existing_age: Duration,
    ) -> Result<(), JobMapCapacityExceeded> {
        self.add_job_with_age_inner(batch_envelope, existing_age, false)
            .await
            .map(|_| ())
    }

    /// SYSCOIN: Recovery needs both fail-fast capacity and an explicit completed-ownership result.
    pub async fn try_admit_snark_job_with_age(
        &self,
        batch_envelope: SignedBatchEnvelope<T>,
        existing_age: Duration,
    ) -> Result<SnarkJobAdmission, JobMapCapacityExceeded> {
        assert_eq!(self.prover_stage, ProverStage::Snark);
        self.add_job_with_age_inner(batch_envelope, existing_age, false)
            .await
    }

    // SYSCOIN: Live admission retains upstream backpressure, while restart recovery shares every
    // duplicate/integrity rule but converts a full-map wait into an explicit startup error.
    async fn add_job_with_age_inner(
        &self,
        batch_envelope: SignedBatchEnvelope<T>,
        existing_age: Duration,
        wait_for_space: bool,
    ) -> Result<SnarkJobAdmission, JobMapCapacityExceeded> {
        let batch_number = batch_envelope.batch_number();
        // SYSCOIN: Measure potentially multi-megabyte durable metadata before taking the shared
        // queue lock; the immutable result follows this exact canonical batch through retries.
        let mut new_metadata = JobMetadata::new_from_batch_with_age(&batch_envelope, existing_age);
        if self.prover_stage == ProverStage::Snark {
            new_metadata.durable_snark_batch_json_bytes =
                durable_snark_batch_json_bytes(&batch_envelope.batch)
                    .expect("canonical batch metadata must serialize for durable SNARK admission");
        }
        // SYSCOIN: The global order is completed ownership, startup boundary, FRI rollback
        // reservations, then live jobs. Every capacity wait drops all guards so completion can
        // always free space and notify it.
        let mut ownership = if self.prover_stage == ProverStage::Snark {
            Some(self.completed_ownership.lock().await)
        } else {
            None
        };
        let mut boundary = if self.prover_stage == ProverStage::Snark {
            Some(self.startup_recovery.lock().await)
        } else {
            None
        };
        let mut fri_reservations = if self.prover_stage == ProverStage::Fri {
            Some(self.fri_rollback_reservations.lock().await)
        } else {
            None
        };
        let mut jobs = self.lock_with_tracking(JobMapMethod::AddJob).await;

        loop {
            // Startup rehydration intentionally runs before the recreated pipeline is drained,
            // so the same canonical batch can arrive here twice. Treat that second arrival as an
            // idempotent replay: replacing the entry would reset its aggregation clock and,
            // worse, could erase a lease that an external prover already picked up. This check
            // belongs inside the loop because another waiter can insert the batch while this
            // caller is blocked on queue capacity.
            if let Some(existing) = jobs.get_mut(&batch_number) {
                // A same-number batch with different authoritative metadata is not an idempotent
                // replay. This invariant is checked after committed-provider validation, so
                // continuing with either value would hide corrupt or contradictory pipeline
                // state. Stop the owning task instead of reporting a successful enqueue.
                assert!(
                    existing.metadata.batch_metadata_digest == new_metadata.batch_metadata_digest,
                    "conflicting same-number prover job metadata for batch {batch_number} at {:?} stage",
                    self.prover_stage
                );

                if new_metadata.added_at < existing.metadata.added_at {
                    existing.metadata.added_at = new_metadata.added_at;
                }

                tracing::info!(
                    batch_number,
                    assigned_to_prover_id = ?existing.metadata.assigned_to_prover_id,
                    ?self.prover_stage,
                    "Ignored duplicate prover job replay while preserving existing queue state"
                );
                return Ok(SnarkJobAdmission::Duplicate);
            }

            // SYSCOIN: A completed real proof awaiting durable/downstream disposition still owns
            // this canonical batch and its endpoint capacity. Treat an identical replay as a
            // duplicate; a contradictory same-number batch remains a fail-hard invariant breach.
            if let Some(record) = fri_reservations
                .as_ref()
                .and_then(|reservations| reservations.get(&batch_number))
            {
                assert!(
                    record.batch_metadata_digest == new_metadata.batch_metadata_digest,
                    "conflicting same-number prover job metadata for reserved FRI batch {batch_number}"
                );
                tracing::info!(
                    batch_number,
                    ?self.prover_stage,
                    "Ignored duplicate prover job replay while durable FRI handoff owns the batch"
                );
                return Ok(SnarkJobAdmission::Duplicate);
            }

            if ownership
                .as_ref()
                .is_some_and(|ownership| ownership.contains(batch_number))
            {
                tracing::info!(
                    batch_number,
                    ?self.prover_stage,
                    "Ignored SNARK FRI replay already owned by a completed wrapper"
                );
                return Ok(SnarkJobAdmission::AlreadyOwned);
            }

            // Wait until there's space available (await if batch range limit would be exceeded).
            // SYSCOIN: Evaluate the prospective span including this exact batch. Looking only at
            // the existing span admits sparse out-of-range recovery jobs and rejects safe interior
            // fills when the endpoints already sit exactly at the configured bound.
            let Some(capacity_error) =
                self.capacity_error_for(&jobs, fri_reservations.as_deref(), batch_number)
            else {
                break;
            };
            if !wait_for_space {
                return Err(capacity_error);
            }
            let queue_statistics = self.compute_and_record_statistics(&jobs);

            tracing::info!(
                batch_number,
                ?queue_statistics,
                ?self.prover_stage,
                max_assigned_batch_range = self.max_assigned_batch_range,
                "Waiting for space in job map"
            );
            // Create notified future while holding lock to avoid missing notifications
            let notified = self.space_available.notified();
            // Drop every lock before awaiting notification. Reacquire in the one global order.
            drop(jobs);
            drop(fri_reservations.take());
            drop(boundary.take());
            drop(ownership.take());
            notified.await;
            ownership = if self.prover_stage == ProverStage::Snark {
                Some(self.completed_ownership.lock().await)
            } else {
                None
            };
            boundary = if self.prover_stage == ProverStage::Snark {
                Some(self.startup_recovery.lock().await)
            } else {
                None
            };
            fri_reservations = if self.prover_stage == ProverStage::Fri {
                Some(self.fri_rollback_reservations.lock().await)
            } else {
                None
            };
            jobs = self.lock_with_tracking(JobMapMethod::AddJob).await;
        }

        let entry = JobEntry {
            metadata: new_metadata,
            batch_envelope,
        };

        if let Some(boundary) = boundary.as_mut() {
            // SYSCOIN: Atomically extend a deferred real startup tip only when its first
            // contiguous live successor is actually admitted to the bounded queue.
            boundary.observe_admission(batch_number);
        }
        jobs.insert(batch_number, entry);

        tracing::info!(
            batch_number,
            queue_statistics = ?self.compute_and_record_statistics(&jobs),
            ?self.prover_stage,
            "Job added"
        );
        Ok(SnarkJobAdmission::Inserted)
    }

    /// SYSCOIN: Release one exact completed-FRI capacity fence after downstream ownership has
    /// accepted the durable proof. A stale guard cannot consume another handoff's reservation.
    async fn release_fri_rollback_reservation(&self, batch_number: u64, token: &ProverLeaseToken) {
        assert_eq!(self.prover_stage, ProverStage::Fri);
        let mut reservations = self.fri_rollback_reservations.lock().await;
        take_fri_rollback_reservation(&mut reservations, batch_number, token);
        drop(reservations);
        self.space_available.notify_waiters();
    }

    /// SYSCOIN: Atomically exchange one exact completed-FRI reservation for its original live
    /// job. The reservation gate prevents ordinary admission from filling the endpoint gap while
    /// rollback validates canonical identity and acquires the queue lock.
    async fn restore_reserved_fri_job(
        &self,
        batch_number: u64,
        token: &ProverLeaseToken,
        batch_envelope: SignedBatchEnvelope<T>,
    ) {
        assert_eq!(self.prover_stage, ProverStage::Fri);
        assert_eq!(batch_envelope.batch_number(), batch_number);
        let restored_metadata = JobMetadata::new_from_batch(&batch_envelope);

        let mut reservations = self.fri_rollback_reservations.lock().await;
        let reservation = reservations
            .get(&batch_number)
            .expect("exact FRI rollback reservation disappeared before restore");
        assert_eq!(
            &reservation.token, token,
            "exact FRI rollback reservation changed before restore"
        );
        assert_eq!(
            reservation.batch_metadata_digest, restored_metadata.batch_metadata_digest,
            "reserved FRI rollback metadata changed for batch {batch_number}"
        );
        let mut jobs = self.lock_with_tracking(JobMapMethod::AddJob).await;

        let entry = JobEntry {
            metadata: restored_metadata,
            batch_envelope,
        };
        let replaced = jobs.insert(batch_number, entry);
        debug_assert!(replaced.is_none(), "job map changed while mutex was held");
        take_fri_rollback_reservation(&mut reservations, batch_number, token);
        tracing::warn!(
            batch_number,
            "Restored exact job from delayed FRI handoff reservation"
        );
    }

    /// Picks the first job (lowest batch number) that is either:
    /// - Pending and older than min_age (fake provers use non-empty min_age)
    /// - Assigned and timed out
    ///
    /// Returns None if no eligible job is found.
    ///
    /// Used for FRI jobs (one batch == one job)
    ///
    /// SYSCOIN: Assignment and returned opaque capability are created under the same map lock.
    pub async fn pick_job<F>(
        &self,
        min_age: Duration,
        prover_id: &str,
        mut predicate: F,
    ) -> Option<LeasedJob<T>>
    where
        F: FnMut(&JobEntry<T>) -> bool,
    {
        let now = Instant::now();
        let (mut jobs, lease_token) = self
            .pick_jobs_while_with_limit_leased(1, prover_id, |entry| {
                // min_age is non-zero only for fake provers
                // for real provers this is no-op - that is, we always take the oldest eligible job
                now.duration_since(entry.metadata.added_at) >= min_age && predicate(entry)
            })
            .await?;
        let (job, data) = jobs.pop().expect("single-job lease must contain one job");
        Some(LeasedJob {
            job,
            data,
            lease_token,
        })
    }

    /// Picks multiple consecutive jobs that satisfy the predicate.
    /// Only returns consecutive batch ranges with no gaps, and all jobs must have the same prover_version.
    ///
    /// The predicate receives (batch_number, &JobEntry<T>) and should return true for jobs that should be picked.
    ///
    /// For FRI jobs, used with `limit = 1`
    /// For SNARK jobs, used with `limit = max_fri_per_snark`
    ///
    /// Returns empty Vec if no eligible jobs are found.
    #[cfg(test)]
    pub async fn pick_jobs_while_with_limit<F>(
        &self,
        limit: usize,
        prover_id: &str,
        predicate: F,
    ) -> Vec<(FriJob, T)>
    where
        F: FnMut(&JobEntry<T>) -> bool,
    {
        self.pick_jobs_while_with_limit_leased(limit, prover_id, predicate)
            .await
            .map_or_else(Vec::new, |(jobs, _lease_token)| jobs)
    }

    /// SYSCOIN: Internal delayed workers must retain the same exact capability discipline as HTTP
    /// provers; otherwise a timed-out fake assignment can consume a newer real prover lease.
    pub(crate) async fn pick_leased_jobs_while_with_limit<F>(
        &self,
        limit: usize,
        prover_id: &str,
        predicate: F,
    ) -> Option<LeasedJobs<T>>
    where
        F: FnMut(&JobEntry<T>) -> bool,
    {
        self.pick_jobs_while_with_limit_leased(limit, prover_id, predicate)
            .await
            .map(|(jobs, lease_token)| LeasedJobs { jobs, lease_token })
    }

    /// SYSCOIN: Assign one OS-random capability atomically with the complete selected range.
    async fn pick_jobs_while_with_limit_leased<F>(
        &self,
        limit: usize,
        prover_id: &str,
        mut predicate: F,
    ) -> Option<(Vec<(FriJob, T)>, String)>
    where
        F: FnMut(&JobEntry<T>) -> bool,
    {
        let now = Instant::now();
        let boundary = if self.prover_stage == ProverStage::Snark {
            Some(self.startup_recovery.lock().await)
        } else {
            None
        };
        let mut jobs = self.lock_with_tracking(JobMapMethod::PickJobsWhile).await;

        let mut selected_jobs = Vec::new();
        if boundary
            .as_ref()
            .is_some_and(|boundary| boundary.phase() != StartupRecoveryPhase::Live)
        {
            // SYSCOIN: During Loading/Draining, an internal fake worker may consume only the
            // complete exact planned head. It cannot jump missing recovery input or later work.
            let head = boundary.as_ref().and_then(|boundary| boundary.head())?;
            let head_len = usize::try_from(head.len()).ok()?;
            if head_len > limit {
                return None;
            }
            for batch_number in head.batch_from()..=head.batch_to() {
                let entry = jobs.get(&batch_number)?;
                if !self.is_job_eligible(&selected_jobs, entry, now, head_len, &mut predicate) {
                    return None;
                }
                selected_jobs.push(entry.metadata.clone());
            }
        } else {
            for (_, entry) in jobs.iter_mut() {
                if !self.is_job_eligible(&selected_jobs, entry, now, limit, &mut predicate) {
                    if selected_jobs.is_empty() {
                        // We didn't find any jobs yet - continue looking for the first eligible one
                        continue;
                    } else {
                        // We already have some jobs - cannot add more jobs, otherwise we'd have a gap
                        break;
                    }
                }

                selected_jobs.push(entry.metadata.clone());
            }
        }

        if selected_jobs.is_empty() {
            return None;
        }

        let assigned_batch_range = (
            selected_jobs.first().unwrap().batch_number,
            selected_jobs.last().unwrap().batch_number,
        );
        let lease_token = ProverLeaseToken::generate();
        for metadata in &selected_jobs {
            jobs.get_mut(&metadata.batch_number)
                .expect("selected prover job disappeared while holding the queue lock")
                .metadata
                .assign(
                    now,
                    prover_id.to_string(),
                    assigned_batch_range,
                    lease_token.clone(),
                );
        }

        let batch_stats = JobBatchStats::new(&selected_jobs);
        let queue_statistics = self.compute_and_record_statistics(&jobs);
        tracing::info!(
            ?batch_stats,
            ?queue_statistics,
            prover_id,
            ?self.prover_stage,
            "Job assigned",
        );

        let jobs = selected_jobs
            .into_iter()
            .map(|metadata| {
                let entry = jobs.get(&metadata.batch_number).unwrap();
                (
                    FriJob {
                        batch_number: metadata.batch_number,
                        vk_hash: metadata.proving_version.vk_hash().to_string(),
                    },
                    entry.batch_envelope.data.clone(),
                )
            })
            .collect();
        Some((jobs, lease_token.to_wire_value()))
    }

    /// SYSCOIN: Atomically inspects, readiness-gates, and assigns the oldest real SNARK range.
    ///
    /// A real range is assigned only once it contains at least two compatible FRI proofs and
    /// either reaches `target_fris`, its oldest proof reaches `max_wait`, or it contains a V32
    /// InteropCenter bundle whose settlement should not wait for the normal amortization window.
    #[cfg(test)]
    pub async fn pick_ready_snark_jobs<F>(
        &self,
        limit: usize,
        target_fris: usize,
        max_wait: Duration,
        prover_id: &str,
        predicate: F,
    ) -> SnarkJobPick<T>
    where
        F: FnMut(&JobEntry<T>) -> bool,
    {
        let mut predicate = predicate;
        self.pick_ready_snark_jobs_with_limits(
            limit,
            target_fris,
            max_wait,
            MAX_JOURNAL_RECORD_BYTES,
            prover_id,
            |entry| {
                if predicate(entry) {
                    SnarkJobEligibility::Eligible
                } else {
                    SnarkJobEligibility::Incompatible
                }
            },
        )
        .await
    }

    /// SYSCOIN: The SNARK HTTP response cap is a readiness boundary, not a topology mismatch.
    /// Release a two-proof prefix immediately when the next compatible proof crosses that cap;
    /// report a deterministic fatal if even the oldest two-proof prefix cannot fit.
    pub async fn pick_ready_snark_jobs_with_response_capacity<F>(
        &self,
        limit: usize,
        target_fris: usize,
        max_wait: Duration,
        prover_id: &str,
        predicate: F,
    ) -> SnarkJobPick<T>
    where
        F: FnMut(&JobEntry<T>) -> SnarkJobEligibility,
    {
        self.pick_ready_snark_jobs_with_limits(
            limit,
            target_fris,
            max_wait,
            MAX_JOURNAL_RECORD_BYTES,
            prover_id,
            predicate,
        )
        .await
    }

    // SYSCOIN: Keep the durable byte limit explicit in the atomic selection implementation. The
    // production wrapper supplies the hard cap; focused tests use smaller limits at exact edges.
    #[cfg(test)]
    async fn pick_ready_snark_jobs_with_journal_limit<F>(
        &self,
        limit: usize,
        target_fris: usize,
        max_wait: Duration,
        journal_record_limit: usize,
        prover_id: &str,
        predicate: F,
    ) -> SnarkJobPick<T>
    where
        F: FnMut(&JobEntry<T>) -> bool,
    {
        let mut predicate = predicate;
        self.pick_ready_snark_jobs_with_limits(
            limit,
            target_fris,
            max_wait,
            journal_record_limit,
            prover_id,
            |entry| {
                if predicate(entry) {
                    SnarkJobEligibility::Eligible
                } else {
                    SnarkJobEligibility::Incompatible
                }
            },
        )
        .await
    }

    async fn pick_ready_snark_jobs_with_limits<F>(
        &self,
        limit: usize,
        target_fris: usize,
        max_wait: Duration,
        journal_record_limit: usize,
        prover_id: &str,
        mut predicate: F,
    ) -> SnarkJobPick<T>
    where
        F: FnMut(&JobEntry<T>) -> SnarkJobEligibility,
    {
        assert_eq!(self.prover_stage, ProverStage::Snark);
        assert!(limit >= 2);
        assert!((2..=limit).contains(&target_fris));

        let now = Instant::now();
        let mut boundary = self.startup_recovery.lock().await;
        let mut jobs = self.lock_with_tracking(JobMapMethod::PickJobsWhile).await;
        if boundary.phase() != StartupRecoveryPhase::Live {
            return self.pick_planned_snark_head(
                &mut boundary,
                &mut jobs,
                now,
                limit,
                target_fris,
                max_wait,
                journal_record_limit,
                prover_id,
                &mut predicate,
            );
        }
        let mut candidate_jobs = Vec::<JobMetadata>::new();
        let mut candidate_batch_json_bytes = 0usize;
        let mut capacity_limited = false;

        for entry in jobs.values() {
            // SYSCOIN: A timed-out assignment cannot be stolen while its admitted verifier owns
            // `submission_in_progress`; only exact release/completion reopens assignment.
            let is_assignable = !entry.metadata.submission_in_progress
                && match entry.metadata.assigned_at {
                    None => true,
                    Some(assigned_at) => now.duration_since(assigned_at) >= self.assignment_timeout,
                };

            if candidate_jobs.is_empty() {
                if !is_assignable {
                    continue;
                }
            } else {
                if candidate_jobs.len() >= limit {
                    break;
                }

                let last = candidate_jobs.last().unwrap();
                if last.batch_number + 1 != entry.metadata.batch_number {
                    break;
                }

                if entry.metadata.proving_version != last.proving_version || !is_assignable {
                    break;
                }
            }

            let batch_from = candidate_jobs
                .first()
                .map_or(entry.metadata.batch_number, |metadata| {
                    metadata.batch_number
                });
            match predicate(entry) {
                SnarkJobEligibility::Eligible => {}
                SnarkJobEligibility::Incompatible if candidate_jobs.is_empty() => continue,
                SnarkJobEligibility::Incompatible => break,
                SnarkJobEligibility::ResponseCapacityExceeded {
                    required_bytes,
                    max_bytes,
                } if candidate_jobs.len() < 2 => {
                    return SnarkJobPick::UnservableResponse {
                        batch_from,
                        blocked_at: entry.metadata.batch_number,
                        required_bytes,
                        max_bytes,
                    };
                }
                SnarkJobEligibility::ResponseCapacityExceeded { .. } => {
                    capacity_limited = true;
                    break;
                }
            }

            let next_batch_json_bytes = candidate_batch_json_bytes
                .saturating_add(entry.metadata.durable_snark_batch_json_bytes);
            let required_bytes = durable_snark_record_json_upper_bound(
                batch_from,
                entry.metadata.batch_number,
                candidate_jobs.len() + 1,
                next_batch_json_bytes,
            )
            .unwrap_or(usize::MAX);
            if required_bytes > journal_record_limit {
                if candidate_jobs.len() < 2 {
                    return SnarkJobPick::Unpersistable {
                        batch_from,
                        blocked_at: entry.metadata.batch_number,
                        required_bytes,
                        max_bytes: journal_record_limit,
                    };
                }
                capacity_limited = true;
                break;
            }
            candidate_batch_json_bytes = next_batch_json_bytes;
            candidate_jobs.push(entry.metadata.clone());
        }

        let Some(oldest_candidate) = candidate_jobs.first() else {
            return SnarkJobPick::Empty;
        };
        let oldest_eligible_age = now.duration_since(oldest_candidate.added_at);
        // SYSCOIN: Preserve Airbender's two-FRI minimum, but let a contiguous compatible range
        // carrying a V32 InteropCenter bundle bypass only the target/age aggregation delay.
        let contains_interop_bundle = candidate_jobs
            .iter()
            .any(|metadata| metadata.contains_interop_bundle);
        let ready = candidate_jobs.len() >= 2
            && (candidate_jobs.len() >= target_fris
                || oldest_eligible_age >= max_wait
                || contains_interop_bundle
                || capacity_limited);

        if !ready {
            return SnarkJobPick::Waiting(SnarkReadinessWait {
                eligible_fris: candidate_jobs.len(),
                oldest_eligible_age,
            });
        }

        let assigned_batch_range = (
            candidate_jobs.first().unwrap().batch_number,
            candidate_jobs.last().unwrap().batch_number,
        );
        let lease_token = ProverLeaseToken::generate();
        for metadata in &candidate_jobs {
            jobs.get_mut(&metadata.batch_number)
                .expect("candidate SNARK job disappeared while holding the queue lock")
                .metadata
                .assign(
                    now,
                    prover_id.to_string(),
                    assigned_batch_range,
                    lease_token.clone(),
                );
        }

        let batch_stats = JobBatchStats::new(&candidate_jobs);
        let queue_statistics = self.compute_and_record_statistics(&jobs);
        tracing::info!(
            ?batch_stats,
            ?queue_statistics,
            prover_id,
            ?self.prover_stage,
            target_fris,
            max_wait_seconds = max_wait.as_secs(),
            contains_interop_bundle,
            capacity_limited,
            journal_record_bytes = durable_snark_record_json_upper_bound(
                assigned_batch_range.0,
                assigned_batch_range.1,
                candidate_jobs.len(),
                candidate_batch_json_bytes,
            ),
            journal_record_limit,
            "Ready SNARK job assigned",
        );

        SnarkJobPick::Assigned {
            jobs: candidate_jobs
                .into_iter()
                .map(|metadata| {
                    let entry = jobs.get(&metadata.batch_number).unwrap();
                    (
                        FriJob {
                            batch_number: metadata.batch_number,
                            vk_hash: metadata.proving_version.vk_hash().to_string(),
                        },
                        entry.batch_envelope.data.clone(),
                    )
                })
                .collect(),
            lease_token: lease_token.to_wire_value(),
        }
    }

    /// SYSCOIN: Loading/Draining exposes only the fully loaded numeric head. Count planning is
    /// stable, while byte caps may lease a viable prefix; completion later shrinks this same head.
    #[allow(clippy::too_many_arguments)]
    fn pick_planned_snark_head<F>(
        &self,
        boundary: &mut SnarkRecoveryBoundary,
        jobs: &mut BTreeMap<u64, JobEntry<T>>,
        now: Instant,
        limit: usize,
        target_fris: usize,
        max_wait: Duration,
        journal_record_limit: usize,
        prover_id: &str,
        predicate: &mut F,
    ) -> SnarkJobPick<T>
    where
        F: FnMut(&JobEntry<T>) -> SnarkJobEligibility,
    {
        let Some(head) = boundary.head() else {
            return SnarkJobPick::Empty;
        };
        let head_len = usize::try_from(head.len())
            .expect("planned SNARK recovery range length must fit usize");
        // SYSCOIN: Runtime recovery must preserve the same wrapper-and-resident-map capacity
        // invariant as startup planning; otherwise it can create a head this map cannot load.
        let effective_recovery_capacity =
            limit.min(self.max_assigned_batch_range.saturating_add(1));
        assert!(
            head_len <= effective_recovery_capacity,
            "planned SNARK recovery range exceeds effective wrapper/map capacity"
        );

        let mut loaded = 0usize;
        let mut oldest_loaded_age = Duration::ZERO;
        for batch_number in head.batch_from()..=head.batch_to() {
            let Some(entry) = jobs.get(&batch_number) else {
                return SnarkJobPick::Waiting(SnarkReadinessWait {
                    eligible_fris: loaded,
                    oldest_eligible_age: oldest_loaded_age,
                });
            };
            let age = now.duration_since(entry.metadata.added_at);
            oldest_loaded_age = oldest_loaded_age.max(age);
            loaded += 1;
        }

        // SYSCOIN: A real wrapper never consumes a singleton. Fake startup recovery may own one,
        // and a deferred real tip remains the exact head until its contiguous successor arrives.
        if head_len < 2 {
            return SnarkJobPick::Waiting(SnarkReadinessWait {
                eligible_fris: loaded,
                oldest_eligible_age: oldest_loaded_age,
            });
        }

        let mut candidate_jobs = Vec::<JobMetadata>::with_capacity(head_len);
        let mut candidate_batch_json_bytes = 0usize;
        let mut capacity_limited = false;
        for batch_number in head.batch_from()..=head.batch_to() {
            let entry = jobs
                .get(&batch_number)
                .expect("fully loaded planned SNARK head disappeared while holding map lock");
            let is_assignable = !entry.metadata.submission_in_progress
                && match entry.metadata.assigned_at {
                    None => true,
                    Some(assigned_at) => now.duration_since(assigned_at) >= self.assignment_timeout,
                };
            let compatible_version = candidate_jobs
                .first()
                .is_none_or(|first| first.proving_version == entry.metadata.proving_version);
            if !is_assignable || !compatible_version {
                return SnarkJobPick::Waiting(SnarkReadinessWait {
                    eligible_fris: candidate_jobs.len(),
                    oldest_eligible_age: oldest_loaded_age,
                });
            }

            match predicate(entry) {
                SnarkJobEligibility::Eligible => {}
                SnarkJobEligibility::Incompatible => {
                    return SnarkJobPick::Waiting(SnarkReadinessWait {
                        eligible_fris: candidate_jobs.len(),
                        oldest_eligible_age: oldest_loaded_age,
                    });
                }
                SnarkJobEligibility::ResponseCapacityExceeded {
                    required_bytes,
                    max_bytes,
                } if candidate_jobs.len() < 2 => {
                    return SnarkJobPick::UnservableResponse {
                        batch_from: head.batch_from(),
                        blocked_at: batch_number,
                        required_bytes,
                        max_bytes,
                    };
                }
                SnarkJobEligibility::ResponseCapacityExceeded { .. } => {
                    capacity_limited = true;
                    break;
                }
            }

            let next_batch_json_bytes = candidate_batch_json_bytes
                .saturating_add(entry.metadata.durable_snark_batch_json_bytes);
            let required_bytes = durable_snark_record_json_upper_bound(
                head.batch_from(),
                batch_number,
                candidate_jobs.len() + 1,
                next_batch_json_bytes,
            )
            .unwrap_or(usize::MAX);
            if required_bytes > journal_record_limit {
                if candidate_jobs.len() < 2 {
                    return SnarkJobPick::Unpersistable {
                        batch_from: head.batch_from(),
                        blocked_at: batch_number,
                        required_bytes,
                        max_bytes: journal_record_limit,
                    };
                }
                capacity_limited = true;
                break;
            }
            candidate_batch_json_bytes = next_batch_json_bytes;
            candidate_jobs.push(entry.metadata.clone());
        }

        if candidate_jobs.len() < head_len {
            debug_assert!(capacity_limited);
            let remainder = head_len - candidate_jobs.len();
            if remainder == 1 {
                if candidate_jobs.len() > 2 {
                    candidate_jobs.pop();
                } else {
                    let completed_to = candidate_jobs
                        .last()
                        .expect("two-FRI capacity prefix must be non-empty")
                        .batch_number;
                    if !boundary.can_defer_tip_after(completed_to) {
                        // SYSCOIN: A count-planned boundary is not durable ownership. Before the
                        // prefix is leased, move the lone remainder across only the immediately
                        // contiguous pending boundary after its newly adjacent FRI is loaded and
                        // proving-version compatible. A journal ownership hole or version
                        // transition remains an immutable hard boundary.
                        let Some(next) = boundary.next_contiguous_range() else {
                            return SnarkJobPick::Unwrappable {
                                batch_from: head.batch_from(),
                                batch_to: head.batch_to(),
                                fittable_fris: candidate_jobs.len(),
                            };
                        };
                        let proving_version = candidate_jobs
                            .first()
                            .expect("capacity-limited prefix must be non-empty")
                            .proving_version;
                        let Some(next_entry) = jobs.get(&next.batch_from()) else {
                            return SnarkJobPick::Waiting(SnarkReadinessWait {
                                eligible_fris: candidate_jobs.len(),
                                oldest_eligible_age: oldest_loaded_age,
                            });
                        };
                        if next_entry.metadata.proving_version != proving_version {
                            return SnarkJobPick::Unwrappable {
                                batch_from: head.batch_from(),
                                batch_to: head.batch_to(),
                                fittable_fris: candidate_jobs.len(),
                            };
                        }
                        if next_entry.metadata.assigned_batch_range.is_some()
                            || next_entry.metadata.submission_in_progress
                        {
                            return SnarkJobPick::Waiting(SnarkReadinessWait {
                                eligible_fris: candidate_jobs.len(),
                                oldest_eligible_age: oldest_loaded_age,
                            });
                        }
                        if !boundary.repartition_head_after_prefix(
                            completed_to,
                            effective_recovery_capacity,
                        ) {
                            return SnarkJobPick::Unwrappable {
                                batch_from: head.batch_from(),
                                batch_to: head.batch_to(),
                                fittable_fris: candidate_jobs.len(),
                            };
                        }
                    }
                }
            }
            candidate_batch_json_bytes = candidate_jobs.iter().fold(0usize, |total, metadata| {
                total.saturating_add(metadata.durable_snark_batch_json_bytes)
            });
        }

        let assigned_batch_range = (
            candidate_jobs
                .first()
                .expect("planned real SNARK head must contain at least two FRIs")
                .batch_number,
            candidate_jobs.last().unwrap().batch_number,
        );
        let lease_token = ProverLeaseToken::generate();
        for metadata in &candidate_jobs {
            jobs.get_mut(&metadata.batch_number)
                .expect("planned SNARK job disappeared while holding the queue lock")
                .metadata
                .assign(
                    now,
                    prover_id.to_string(),
                    assigned_batch_range,
                    lease_token.clone(),
                );
        }

        let batch_stats = JobBatchStats::new(&candidate_jobs);
        let queue_statistics = self.compute_and_record_statistics(jobs);
        tracing::info!(
            ?batch_stats,
            ?queue_statistics,
            prover_id,
            ?self.prover_stage,
            target_fris,
            max_wait_seconds = max_wait.as_secs(),
            capacity_limited,
            startup_recovery_phase = ?boundary.phase(),
            planned_batch_from = head.batch_from(),
            planned_batch_to = head.batch_to(),
            journal_record_bytes = durable_snark_record_json_upper_bound(
                assigned_batch_range.0,
                assigned_batch_range.1,
                candidate_jobs.len(),
                candidate_batch_json_bytes,
            ),
            journal_record_limit,
            "Planned startup SNARK prefix assigned",
        );

        SnarkJobPick::Assigned {
            jobs: candidate_jobs
                .into_iter()
                .map(|metadata| {
                    let entry = jobs.get(&metadata.batch_number).unwrap();
                    (
                        FriJob {
                            batch_number: metadata.batch_number,
                            vk_hash: metadata.proving_version.vk_hash().to_string(),
                        },
                        entry.batch_envelope.data.clone(),
                    )
                })
                .collect(),
            lease_token: lease_token.to_wire_value(),
        }
    }

    pub async fn has_assignable_job<F>(&self, mut predicate: F) -> bool
    where
        F: FnMut(&JobEntry<T>) -> bool,
    {
        let now = Instant::now();
        let boundary = if self.prover_stage == ProverStage::Snark {
            Some(self.startup_recovery.lock().await)
        } else {
            None
        };
        let jobs = self.lock_with_tracking(JobMapMethod::PickJobsWhile).await;
        if boundary
            .as_ref()
            .is_some_and(|boundary| boundary.phase() != StartupRecoveryPhase::Live)
        {
            let Some(head) = boundary.as_ref().and_then(|boundary| boundary.head()) else {
                return false;
            };
            let Ok(head_len) = usize::try_from(head.len()) else {
                return false;
            };
            let mut selected = Vec::with_capacity(head_len);
            for batch_number in head.batch_from()..=head.batch_to() {
                let Some(entry) = jobs.get(&batch_number) else {
                    return false;
                };
                if !self.is_job_eligible(&selected, entry, now, head_len, &mut predicate) {
                    return false;
                }
                selected.push(entry.metadata.clone());
            }
            true
        } else {
            jobs.values()
                .any(|entry| self.is_job_eligible(&[], entry, now, 1, &mut predicate))
        }
    }

    /// Checks if a job is eligible for assignment based on:
    /// - Not exceeding the limit of selected jobs
    /// - Being either pending or timed out
    /// - Passing the external predicate
    /// - Maintaining consecutive batch numbers and matching proving version
    fn is_job_eligible<F>(
        &self,
        already_selected_jobs: &[JobMetadata],
        next_job_entry: &JobEntry<T>,
        now: Instant,
        limit: usize,
        predicate: &mut F,
    ) -> bool
    where
        F: FnMut(&JobEntry<T>) -> bool,
    {
        // Stop if we've reached the limit
        if already_selected_jobs.len() >= limit {
            return false;
        }

        // Job is either pending or timed out
        // SYSCOIN: Do not reassign a timed-out job while exact-token verification is in progress.
        let is_assignable = !next_job_entry.metadata.submission_in_progress
            && match next_job_entry.metadata.assigned_at {
                None => true,
                Some(assigned_at) => now.duration_since(assigned_at) >= self.assignment_timeout,
            };
        if !is_assignable {
            return false;
        }

        // Predicate passed from outside should return `true`
        if !predicate(next_job_entry) {
            return false;
        }

        // No gaps in batch numbers and all have the same proving version
        match already_selected_jobs.last() {
            None => true,
            Some(last) => {
                last.batch_number + 1 == next_job_entry.metadata.batch_number
                    && next_job_entry.metadata.proving_version == last.proving_version
            }
        }
    }

    /// SYSCOIN: Atomically authenticate and claim one exact external submission before any
    /// prover-controlled payload reaches a cryptographic verifier.
    pub async fn begin_submission(
        self: &Arc<Self>,
        batch_number_from: u64,
        batch_number_to: u64,
        lease_token: &str,
    ) -> Result<SubmissionLease<T>, BeginSubmissionError>
    where
        T: Send + 'static,
    {
        if batch_number_from > batch_number_to {
            return Err(BeginSubmissionError::InvalidRange);
        }
        let mut jobs = self
            .lock_with_tracking(JobMapMethod::GetJobBatchMetadata)
            .await;
        let Some(first) = jobs.get(&batch_number_from) else {
            return Err(BeginSubmissionError::UnknownJob);
        };
        let requested_range = (batch_number_from, batch_number_to);
        if first.metadata.assigned_batch_range != Some(requested_range)
            || !first
                .metadata
                .assigned_lease_token
                .as_ref()
                .is_some_and(|token| token.matches_wire_value(lease_token))
        {
            return Err(BeginSubmissionError::InvalidLease);
        }
        let token = first
            .metadata
            .assigned_lease_token
            .clone()
            .expect("validated assigned lease token disappeared");

        let mut batch_snapshots = Vec::new();
        for batch_number in batch_number_from..=batch_number_to {
            let Some(entry) = jobs.get(&batch_number) else {
                return Err(BeginSubmissionError::UnknownJob);
            };
            if entry.metadata.assigned_batch_range != Some(requested_range)
                || entry.metadata.assigned_lease_token.as_ref() != Some(&token)
            {
                return Err(BeginSubmissionError::InvalidLease);
            }
            if entry.metadata.submission_in_progress {
                return Err(BeginSubmissionError::AlreadySubmitting);
            }
            // SYSCOIN: FRI admits one full verifier snapshot. SNARK admits only immutable versions,
            // avoiding a lock-held clone of logs/messages/signatures across a 100-batch aggregate.
            let needs_fri_snapshot = self.prover_stage == ProverStage::Fri;
            batch_snapshots.push(SubmissionBatchSnapshot {
                proving_version: entry.metadata.proving_version,
                // SYSCOIN: Preserve the exact admitted batch identity for a later atomic FRI
                // completion-to-rollback-reservation transition.
                batch_metadata_digest: entry.metadata.batch_metadata_digest,
                fri_batch: needs_fri_snapshot.then(|| entry.batch_envelope.batch.clone()),
                fri_signature_data: needs_fri_snapshot
                    .then(|| entry.batch_envelope.signature_data.clone()),
            });
        }

        for batch_number in batch_number_from..=batch_number_to {
            jobs.get_mut(&batch_number)
                .expect("validated submission job disappeared while holding the queue lock")
                .metadata
                .submission_in_progress = true;
        }

        Ok(SubmissionLease {
            jobs: Arc::clone(self),
            batch_range: requested_range,
            token,
            batch_snapshots,
            active: true,
        })
    }

    /// SYSCOIN: Release only this exact in-progress capability, retaining the assignment for retry.
    async fn release_submission(&self, batch_range: (u64, u64), token: &ProverLeaseToken) {
        let mut jobs = self.lock_with_tracking(JobMapMethod::UnassignJob).await;
        for batch_number in batch_range.0..=batch_range.1 {
            let Some(entry) = jobs.get(&batch_number) else {
                return;
            };
            if entry.metadata.assigned_batch_range != Some(batch_range)
                || entry.metadata.assigned_lease_token.as_ref() != Some(token)
                || !entry.metadata.submission_in_progress
            {
                return;
            }
        }
        for batch_number in batch_range.0..=batch_range.1 {
            jobs.get_mut(&batch_number)
                .expect("validated submission job disappeared while holding the queue lock")
                .metadata
                .submission_in_progress = false;
        }
    }

    /// SYSCOIN: Preserve exact expensive-proof ownership across a retryable server-side handoff
    /// failure. The opaque token is revalidated for every batch before refreshing its clock.
    async fn release_submission_for_retry(
        &self,
        batch_range: (u64, u64),
        token: &ProverLeaseToken,
    ) {
        let mut jobs = self.lock_with_tracking(JobMapMethod::UnassignJob).await;
        for batch_number in batch_range.0..=batch_range.1 {
            let Some(entry) = jobs.get(&batch_number) else {
                return;
            };
            if entry.metadata.assigned_batch_range != Some(batch_range)
                || entry.metadata.assigned_lease_token.as_ref() != Some(token)
                || !entry.metadata.submission_in_progress
            {
                return;
            }
        }
        let refreshed_at = Instant::now();
        for batch_number in batch_range.0..=batch_range.1 {
            let metadata = &mut jobs
                .get_mut(&batch_number)
                .expect("validated retry submission disappeared while holding the queue lock")
                .metadata;
            metadata.submission_in_progress = false;
            metadata.assigned_at = Some(refreshed_at);
        }
    }

    /// SYSCOIN: A definitive owner rejection revokes only the exact capability that was verified.
    async fn revoke_submission(&self, batch_range: (u64, u64), token: &ProverLeaseToken) {
        let mut jobs = self.lock_with_tracking(JobMapMethod::UnassignJob).await;
        for batch_number in batch_range.0..=batch_range.1 {
            let Some(entry) = jobs.get(&batch_number) else {
                return;
            };
            if entry.metadata.assigned_batch_range != Some(batch_range)
                || entry.metadata.assigned_lease_token.as_ref() != Some(token)
                || !entry.metadata.submission_in_progress
            {
                return;
            }
        }
        for batch_number in batch_range.0..=batch_range.1 {
            jobs.get_mut(&batch_number)
                .expect("validated submission job disappeared while holding the queue lock")
                .metadata
                .unassign();
        }
        tracing::info!(
            batch_number_from = batch_range.0,
            batch_number_to = batch_range.1,
            ?self.prover_stage,
            "Prover lease revoked after rejected owner submission"
        );
    }

    /// If a job is present for a given batch_number, returns the corresponding BatchMetadata
    #[cfg(test)]
    pub async fn get_job_batch_metadata(&self, batch_number: u64) -> Option<BatchMetadata> {
        let jobs = self
            .lock_with_tracking(JobMapMethod::GetJobBatchMetadata)
            .await;
        jobs.get(&batch_number)
            .map(|entry| entry.batch_envelope.batch.clone())
    }

    /// If a job is present for given batch_number, returns (vk, prover_input)
    pub async fn get_prover_input(&self, batch_number: u64) -> Option<(&'static str, T)> {
        let jobs = self.lock_with_tracking(JobMapMethod::GetProverInput).await;
        jobs.get(&batch_number).map(|entry| {
            (
                entry
                    .batch_envelope
                    .batch
                    .verification_key_hash()
                    .expect("VK hash must exist"),
                entry.batch_envelope.data.clone(),
            )
        })
    }

    /// SYSCOIN: Complete only the exact capability currently marked submission-in-progress.
    async fn complete_leased_many_jobs(
        &self,
        batch_number_from: u64,
        batch_number_to: u64,
        prover_type: ProverType,
        prover_id: &str,
        lease_token: &ProverLeaseToken,
    ) -> Option<Vec<SignedBatchEnvelope<T>>> {
        match self
            .complete_many_jobs_inner(
                batch_number_from,
                batch_number_to,
                prover_type,
                prover_id,
                lease_token,
                false,
            )
            .await
        {
            SnarkOwnershipCompletion::Completed(completed) => Some(completed),
            SnarkOwnershipCompletion::AlreadyOwned | SnarkOwnershipCompletion::Stale => None,
        }
    }

    /// SYSCOIN: A real journal or fake command claims permanent completed ownership in the same
    /// critical section that validates and removes the exact opaque submission capability.
    async fn complete_leased_many_jobs_with_ownership(
        &self,
        batch_number_from: u64,
        batch_number_to: u64,
        prover_type: ProverType,
        prover_id: &str,
        lease_token: &ProverLeaseToken,
    ) -> SnarkOwnershipCompletion<T> {
        assert_eq!(self.prover_stage, ProverStage::Snark);
        self.complete_many_jobs_inner(
            batch_number_from,
            batch_number_to,
            prover_type,
            prover_id,
            lease_token,
            true,
        )
        .await
    }

    async fn complete_many_jobs_inner(
        &self,
        batch_number_from: u64,
        batch_number_to: u64,
        prover_type: ProverType,
        prover_id: &str,
        submission_token: &ProverLeaseToken,
        claim_completed_ownership: bool,
    ) -> SnarkOwnershipCompletion<T> {
        // SYSCOIN: Inverted external SNARK submit ranges must not reach JobBatchStats::new with
        // an empty metadata list.
        if batch_number_from > batch_number_to {
            tracing::warn!(
                batch_number_from,
                batch_number_to,
                prover_id,
                ?prover_type,
                ?self.prover_stage,
                "Cannot complete jobs: invalid empty batch range"
            );
            return SnarkOwnershipCompletion::Stale;
        }

        // SYSCOIN: Never acquire completed ownership while holding the live map. This single
        // global order is also used by admission, restore, and recovery seeding.
        let mut ownership = if claim_completed_ownership {
            Some(self.completed_ownership.lock().await)
        } else {
            None
        };
        let mut boundary = if claim_completed_ownership {
            Some(self.startup_recovery.lock().await)
        } else {
            None
        };
        let mut jobs = self
            .lock_with_tracking(JobMapMethod::CompleteManyJobs)
            .await;
        if ownership
            .as_ref()
            .is_some_and(|ownership| ownership.overlaps(batch_number_from, batch_number_to))
        {
            tracing::warn!(
                batch_number_from,
                batch_number_to,
                prover_id,
                ?prover_type,
                "Cannot complete SNARK jobs: range already has completed ownership"
            );
            return SnarkOwnershipCompletion::AlreadyOwned;
        }
        if boundary.as_ref().is_some_and(|boundary| {
            !boundary.can_complete_head((batch_number_from, batch_number_to))
        }) {
            tracing::warn!(
                batch_number_from,
                batch_number_to,
                prover_id,
                ?prover_type,
                startup_recovery_head = ?boundary.as_ref().and_then(|boundary| boundary.head()).map(PlannedSnarkRange::as_tuple),
                "Cannot complete SNARK jobs: capability does not start at the recovery head"
            );
            return SnarkOwnershipCompletion::Stale;
        }
        // First, verify all jobs exist -
        // it's possible a different job with an overlapping set of proofs was submitted.
        for batch_number in batch_number_from..=batch_number_to {
            let Some(entry) = jobs.get(&batch_number) else {
                tracing::warn!(
                    batch_number_from,
                    batch_number_to,
                    missing_batch_number = batch_number,
                    prover_id,
                    ?prover_type,
                    ?self.prover_stage,
                    "Cannot complete job: job missing from map (race condition)"
                );
                return SnarkOwnershipCompletion::Stale;
            };
            // SYSCOIN: The bearer capability is authority; the request's public prover ID remains
            // diagnostic and may differ from the pick label.
            if entry.metadata.assigned_batch_range != Some((batch_number_from, batch_number_to)) {
                tracing::warn!(
                    batch_number_from,
                    batch_number_to,
                    rejected_at_batch_number = batch_number,
                    prover_id,
                    assigned_to_prover_id = ?entry.metadata.assigned_to_prover_id,
                    assigned_batch_range = ?entry.metadata.assigned_batch_range,
                    ?self.prover_stage,
                    "Cannot complete jobs: submitted range does not match the current prover assignment"
                );
                return SnarkOwnershipCompletion::Stale;
            }
            if entry.metadata.assigned_lease_token.as_ref() != Some(submission_token)
                || !entry.metadata.submission_in_progress
            {
                tracing::warn!(
                    batch_number_from,
                    batch_number_to,
                    rejected_at_batch_number = batch_number,
                    ?prover_type,
                    ?self.prover_stage,
                    "Cannot complete jobs: opaque submission lease is stale or not in progress"
                );
                return SnarkOwnershipCompletion::Stale;
            }
        }
        // SYSCOIN: Claim before exact-token removal while both guards are held. Admission can now
        // observe only the old live job or the new tombstone, never an unowned empty gap.
        if claim_completed_ownership {
            ownership
                .as_mut()
                .expect("completed ownership claim requires ownership guard")
                .claim(batch_number_from, batch_number_to);
            assert!(
                boundary
                    .as_mut()
                    .expect("completed ownership claim requires startup-boundary guard")
                    .complete_head((batch_number_from, batch_number_to)),
                "validated startup recovery head changed while all state locks were held"
            );
        }

        let mut completed = Vec::new();
        for batch_number in batch_number_from..=batch_number_to {
            let entry = jobs.remove(&batch_number).unwrap();
            completed.push(entry);
        }

        let metadata: Vec<JobMetadata> = completed.iter().map(|e| e.metadata.clone()).collect();
        let stats = JobBatchStats::new(&metadata);

        tracing::info!(
            ?stats,
            ?prover_type,
            prover_id,
            ?self.prover_stage,
            queue_statistics = ?self.compute_and_record_statistics(&jobs),
            "Job completed",
        );

        drop(jobs);
        drop(boundary);
        drop(ownership);
        // Notify once for all completed jobs
        self.space_available.notify_waiters();

        // Record Prometheus metrics
        match &stats.job_with_max_attempts_info {
            // SYSCOIN: Exact-token admission authenticates the assignment; prover ID is diagnostic.
            Some(assignment_info) => {
                // SYSCOIN: Collapse capability-authenticated work to one bounded metrics identity.
                // Pick and submit labels remain bounded diagnostics in logs only.
                if assignment_info.last_assigned_to != prover_id {
                    tracing::info!(
                        assigned_prover_id = %assignment_info.last_assigned_to,
                        submitted_prover_id = prover_id,
                        ?self.prover_stage,
                        "Bearer-authenticated proof completed under a different diagnostic prover ID"
                    );
                }
                PROVER_METRICS.prove_time[&(
                    self.prover_stage,
                    prover_type,
                    CAPABILITY_AUTHENTICATED_METRICS_ID.to_string(),
                )]
                    // time since last assignment is proving time
                    .observe(assignment_info.time_since_last_assignment);
                if let Some(total_computational_native_used) = stats.total_computational_native_used
                {
                    PROVER_METRICS.computational_native_proven[&(
                        self.prover_stage,
                        prover_type,
                        CAPABILITY_AUTHENTICATED_METRICS_ID.to_string(),
                    )]
                        .observe(total_computational_native_used);
                    if total_computational_native_used > 0 {
                        PROVER_METRICS.prove_time_per_million_native[&(
                            self.prover_stage,
                            prover_type,
                            CAPABILITY_AUTHENTICATED_METRICS_ID.to_string(),
                        )]
                            .observe(
                                assignment_info
                                    .time_since_last_assignment
                                    .div_f64(total_computational_native_used as f64 / 1_000_000.0),
                            );
                    }
                }
                if stats.total_txs > 0 {
                    PROVER_METRICS.prove_time_per_tx[&(
                        self.prover_stage,
                        prover_type,
                        CAPABILITY_AUTHENTICATED_METRICS_ID.to_string(),
                    )]
                        .observe(
                            assignment_info.time_since_last_assignment / stats.total_txs as u32,
                        );
                }
                PROVER_METRICS.proved_after_attempts[&(self.prover_stage, prover_type)]
                    .observe(assignment_info.attempts as f64);
            }
            None => {
                tracing::info!(
                    ?stats,
                    ?self.prover_stage,
                    "Received a valid proof for a job not marked as assigned - possibly assigned before a restart."
                )
            }
        }

        SnarkOwnershipCompletion::Completed(
            completed
                .into_iter()
                .map(|entry| entry.batch_envelope)
                .collect(),
        )
    }

    /// SYSCOIN: Return the current bounded-map state only when inserting `batch_number` would make
    /// the prospective endpoint difference exceed `max_assigned_batch_range`. Equality is valid.
    fn capacity_error_for(
        &self,
        jobs: &BTreeMap<u64, JobEntry<T>>,
        fri_reservations: Option<&BTreeMap<u64, FriRollbackReservationRecord>>,
        batch_number: u64,
    ) -> Option<JobMapCapacityExceeded> {
        let jobs_min = jobs.keys().next().copied();
        let jobs_max = jobs.keys().next_back().copied();
        let reserved_min =
            fri_reservations.and_then(|reservations| reservations.keys().next().copied());
        let reserved_max =
            fri_reservations.and_then(|reservations| reservations.keys().next_back().copied());
        let current_min = match (jobs_min, reserved_min) {
            (Some(jobs_min), Some(reserved_min)) => jobs_min.min(reserved_min),
            (Some(jobs_min), None) => jobs_min,
            (None, Some(reserved_min)) => reserved_min,
            (None, None) => return None,
        };
        let current_max = match (jobs_max, reserved_max) {
            (Some(jobs_max), Some(reserved_max)) => jobs_max.max(reserved_max),
            (Some(jobs_max), None) => jobs_max,
            (None, Some(reserved_max)) => reserved_max,
            (None, None) => return None,
        };
        let prospective_min = current_min.min(batch_number);
        let prospective_max = current_max.max(batch_number);
        (prospective_max - prospective_min > self.max_assigned_batch_range as u64).then_some(
            JobMapCapacityExceeded {
                batch_number,
                current_min,
                current_max,
                max_assigned_batch_range: self.max_assigned_batch_range,
                prover_stage: self.prover_stage,
            },
        )
    }

    fn compute_and_record_statistics(&self, jobs: &BTreeMap<u64, JobEntry<T>>) -> QueueStatistics {
        let min_batch = jobs.values().next();
        PROVER_METRICS.batch_count[&self.prover_stage].set(jobs.len() as i64);
        match min_batch {
            Some(min_batch) => {
                let min_batch_number = min_batch.batch_envelope.batch_number();
                let max_batch_number = *jobs.keys().next_back().unwrap();
                let result = QueueStatistics::NonEmpty(NonEmptyQueueStatistics {
                    min_batch_added_at: min_batch.metadata.added_at,
                    min_batch_current_attempt: min_batch.metadata.current_attempt,
                    min_batch_number: min_batch.batch_envelope.batch_number(),
                    max_batch_number,
                    jobs_count: jobs.len(),
                });
                PROVER_METRICS.prover_job_map_min_batch_number[&self.prover_stage]
                    .set(min_batch_number as i64);
                PROVER_METRICS.prover_job_map_max_batch_number[&self.prover_stage]
                    .set(max_batch_number as i64);
                result
            }
            None => QueueStatistics::Empty,
        }
    }

    pub async fn status(&self) -> Vec<JobState> {
        let jobs = self.lock_with_tracking(JobMapMethod::Status).await;
        jobs.iter()
            .map(|(batch_number, entry)| JobState {
                fri_job: FriJob {
                    batch_number: *batch_number,
                    vk_hash: entry.metadata.proving_version.vk_hash().to_string(),
                },
                assigned_seconds_ago: entry
                    .metadata
                    .assigned_at
                    .map(|assigned_at| assigned_at.elapsed().as_secs()),
                current_attempt: entry.metadata.current_attempt,
                assigned_to_prover_id: entry
                    .metadata
                    .assigned_to_prover_id
                    .as_ref()
                    .map(|id| id.to_string()),
                added_seconds_ago: entry.metadata.added_at.elapsed().as_secs(),
            })
            .collect() // Already sorted by BTreeMap ordering
    }

    const WARN_AT_ACQUIRE_TIME_MS: u64 = 500;
    /// Acquire the lock with tracking of acquisition time and hold time
    async fn lock_with_tracking(&self, method: JobMapMethod) -> TrackedLockGuard<'_, T> {
        let start = Instant::now();
        let guard = self.jobs.lock().await;
        let acquire_time = start.elapsed();
        if acquire_time > Duration::from_millis(Self::WARN_AT_ACQUIRE_TIME_MS) {
            tracing::warn!(
                acquire_time_ms = acquire_time.as_millis(),
                ?method,
                ?self.prover_stage,
                "Contention on job map lock"
            );
        }

        PROVER_METRICS.job_map_lock_acquire_time[&(self.prover_stage, method)]
            .observe(acquire_time);

        TrackedLockGuard::new(guard, Instant::now(), self.prover_stage, method)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prover_api::metrics::ProverStage;
    // SYSCOIN: These fixtures cover V32 interop metadata, compact edge-DA commitments, and the
    // bounded FRI-to-SNARK ownership/recovery paths.
    use crate::prover_api::test_util::mark_test_batch_as_interop_bundle;
    use alloy::primitives::{Address, B256, keccak256};
    use std::time::Duration;
    use zksync_os_batch_types::{
        PendingBatchInfo,
        batcher_model::{BatchForSigning, FriProof},
    };
    use zksync_os_contract_interface::models::{
        CommitBatchInfo, DACommitmentScheme, StoredBatchInfo,
    };
    use zksync_os_types::{ProtocolSemanticVersion, ProvingVersion, PubdataMode};

    fn create_test_batch_envelope(batch_number: u64) -> SignedBatchEnvelope<Vec<u8>> {
        create_test_batch_envelope_with_upgrade(batch_number, None)
    }

    fn create_test_batch_envelope_with_protocol_version(
        batch_number: u64,
        protocol_version: ProtocolSemanticVersion,
    ) -> SignedBatchEnvelope<Vec<u8>> {
        create_test_batch_envelope_with_protocol_version_and_upgrade(
            batch_number,
            protocol_version,
            None,
        )
    }

    fn create_test_batch_envelope_with_upgrade(
        batch_number: u64,
        upgrade_tx_hash: Option<B256>,
    ) -> SignedBatchEnvelope<Vec<u8>> {
        create_test_batch_envelope_with_protocol_version_and_upgrade(
            batch_number,
            // SYSCOIN: Synthetic jobs pin the sole fresh-deployment V32 identity and its V8
            // proving lane instead of inheriting a historical fixture default.
            ProtocolSemanticVersion::new(0, 32, 0),
            upgrade_tx_hash,
        )
    }

    fn create_test_batch_envelope_with_protocol_version_and_upgrade(
        batch_number: u64,
        protocol_version: ProtocolSemanticVersion,
        upgrade_tx_hash: Option<B256>,
    ) -> SignedBatchEnvelope<Vec<u8>> {
        let batch = BatchMetadata {
            previous_stored_batch_info: StoredBatchInfo {
                batch_number: batch_number.saturating_sub(1),
                state_commitment: B256::ZERO,
                number_of_layer1_txs: 0,
                priority_operations_hash: B256::ZERO,
                dependency_roots_rolling_hash: B256::ZERO,
                l2_to_l1_logs_root_hash: B256::ZERO,
                commitment: B256::ZERO,
                // unused
                last_block_timestamp: Some(0),
            },
            batch_info: PendingBatchInfo {
                commit_info: CommitBatchInfo {
                    batch_number,
                    new_state_commitment: B256::ZERO,
                    number_of_layer1_txs: 0,
                    number_of_layer2_txs: 0,
                    priority_operations_hash: B256::ZERO,
                    dependency_roots_rolling_hash: B256::ZERO,
                    l2_to_l1_logs_root_hash: B256::ZERO,
                    l2_da_commitment_scheme: DACommitmentScheme::BlobsZKsyncOS,
                    da_commitment: keccak256([0u8; 32]),
                    first_block_timestamp: 0,
                    first_block_number: Some(batch_number),
                    last_block_timestamp: 0,
                    last_block_number: Some(batch_number),
                    chain_id: 1,
                    operator_da_input: vec![0u8; 32],
                    // SYSCOIN: dummy batches do not include compact edge DA ref openings.
                    edge_da_refs_input: vec![],
                    // SYSCOIN: dummy batches do not include compact edge DA refs.
                    edge_da_refs_root: B256::ZERO,
                    sl_chain_id: 2,
                },
                protocol_version,
                upgrade_tx_hash,
            },
            chain_address: Address::ZERO,
            first_block_number: batch_number,
            last_block_number: batch_number,
            last_block_hash: None,
            pubdata_mode: PubdataMode::Blobs,
            tx_count: 10,
            computational_native_used: None,
            logs: vec![],
            messages: vec![],
            multichain_root: Default::default(),
            set_sl_chain_id_migration_number: None,
        };

        BatchForSigning::new(batch, vec![1, 2, 3])
            .with_signatures(zksync_os_batch_types::batcher_model::BatchSignatureData::NotNeeded)
    }

    // SYSCOIN: Tests remove FRI work through the same opaque-capability flow as the fake prover,
    // never through an unassigned completion shortcut.
    async fn pick_begin_and_complete_fake_fri(
        map: &Arc<ProverJobMap<Vec<u8>>>,
        batch_number: u64,
        prover_id: &str,
    ) -> Option<SignedBatchEnvelope<Vec<u8>>> {
        let picked = map
            .pick_job(Duration::ZERO, prover_id, |entry| {
                entry.metadata.batch_number == batch_number
            })
            .await?;
        let submission = map
            .begin_submission(batch_number, batch_number, &picked.lease_token)
            .await
            .expect("a freshly picked FRI capability must enter submission");
        let mut completed = submission.complete_fake_fri(prover_id).await?;
        let envelope = completed
            .pop()
            .expect("single FRI completion must return one batch envelope");
        debug_assert!(completed.is_empty());
        Some(envelope)
    }

    // SYSCOIN: Exercise the production SNARK ownership boundary from an actual aggregate pick.
    async fn pick_and_begin_snark(
        map: &Arc<ProverJobMap<Vec<u8>>>,
        batch_number_from: u64,
        batch_number_to: u64,
        prover_id: &str,
    ) -> SubmissionLease<Vec<u8>> {
        let job_count = usize::try_from(batch_number_to - batch_number_from + 1).unwrap();
        assert!(job_count >= 2);
        let picked = map
            .pick_ready_snark_jobs(job_count, job_count, Duration::ZERO, prover_id, |_| true)
            .await;
        let SnarkJobPick::Assigned { jobs, lease_token } = picked else {
            panic!("ready SNARK range must be assigned")
        };
        assert_eq!(
            jobs.iter()
                .map(|(job, _)| job.batch_number)
                .collect::<Vec<_>>(),
            (batch_number_from..=batch_number_to).collect::<Vec<_>>()
        );
        map.begin_submission(batch_number_from, batch_number_to, &lease_token)
            .await
            .expect("a freshly picked SNARK capability must enter submission")
    }

    #[tokio::test]
    async fn test_add_and_complete_job() {
        let map = Arc::new(ProverJobMap::new(
            Duration::from_secs(60),
            100,
            ProverStage::Fri,
        ));

        let envelope = create_test_batch_envelope(1);
        map.add_job(envelope).await;

        let metadata = map.get_job_batch_metadata(1).await;
        assert!(metadata.is_some());
        assert_eq!(metadata.unwrap().batch_info.commit_info.batch_number, 1);

        let result = pick_begin_and_complete_fake_fri(&map, 1, "prover-1").await;
        assert!(result.is_some());
        assert_eq!(result.unwrap().batch_number(), 1);

        let metadata = map.get_job_batch_metadata(1).await;
        assert!(metadata.is_none());
    }

    #[tokio::test]
    async fn concurrent_duplicate_waiters_preserve_earliest_queue_age() {
        let map = std::sync::Arc::new(ProverJobMap::new(
            Duration::from_secs(60),
            2,
            ProverStage::Fri,
        ));
        map.add_job(create_test_batch_envelope(1)).await;
        map.add_job(create_test_batch_envelope(3)).await;

        // Both additions would extend the endpoint difference past the configured range. Once
        // batch 1 is removed they wake together; whichever inserts first, the other must re-check
        // the key under the lock and merge only the earlier age instead of replacing the entry.
        let aged_map = map.clone();
        let aged_add = tokio::spawn(async move {
            aged_map
                .add_job_with_age(create_test_batch_envelope(4), Duration::from_secs(5))
                .await;
        });
        let fresh_map = map.clone();
        let fresh_add = tokio::spawn(async move {
            fresh_map.add_job(create_test_batch_envelope(4)).await;
        });
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(!aged_add.is_finished());
        assert!(!fresh_add.is_finished());

        pick_begin_and_complete_fake_fri(&map, 1, "test")
            .await
            .expect("the head job must be removable");
        tokio::time::timeout(Duration::from_secs(1), async {
            aged_add.await.unwrap();
            fresh_add.await.unwrap();
        })
        .await
        .expect("both duplicate waiters must finish");

        let status = map.status().await;
        let batch_4 = status
            .iter()
            .find(|job| job.fri_job.batch_number == 4)
            .expect("batch 4 must be queued exactly once");
        assert!(batch_4.added_seconds_ago >= 5);
        assert_eq!(
            status
                .iter()
                .filter(|job| job.fri_job.batch_number == 4)
                .count(),
            1
        );
    }

    // SYSCOIN: Restart admission must bound the range after inserting the candidate, while still
    // permitting exact-boundary endpoints, safe interior fills, and idempotent duplicates.
    #[tokio::test]
    async fn recovery_capacity_uses_prospective_span() {
        let map = ProverJobMap::new(Duration::from_secs(60), 2, ProverStage::Fri);
        map.try_add_job_with_age(create_test_batch_envelope(10), Duration::ZERO)
            .await
            .unwrap();

        let error = map
            .try_add_job_with_age(create_test_batch_envelope(13), Duration::ZERO)
            .await
            .expect_err("a sparse candidate beyond the prospective span must be rejected");
        assert_eq!(error.batch_number, 13);
        assert_eq!((error.current_min, error.current_max), (10, 10));
        assert_eq!(error.max_assigned_batch_range, 2);
        assert_eq!(error.prover_stage, ProverStage::Fri);
        assert!(map.get_job_batch_metadata(13).await.is_none());

        map.try_add_job_with_age(create_test_batch_envelope(12), Duration::ZERO)
            .await
            .expect("a span exactly equal to the bound must be admitted");
        map.try_add_job_with_age(create_test_batch_envelope(11), Duration::ZERO)
            .await
            .expect("an interior fill must be admitted at the endpoint bound");
        map.try_add_job_with_age(create_test_batch_envelope(10), Duration::from_secs(5))
            .await
            .expect("an idempotent duplicate must bypass capacity rejection");

        let status = map.status().await;
        let batch_10 = status
            .iter()
            .find(|job| job.fri_job.batch_number == 10)
            .expect("the duplicate batch must remain queued");
        assert!(batch_10.added_seconds_ago >= 5);
        assert_eq!(status.len(), 3);
    }

    // SYSCOIN: Recovery ownership seeding is one transaction: a bad later range or active-job
    // overlap cannot leave an earlier tombstone behind and silently discard future work.
    #[tokio::test]
    async fn recovered_ownership_seeding_is_all_or_nothing() {
        let map = ProverJobMap::new(Duration::from_secs(60), 100, ProverStage::Snark);
        assert_eq!(
            map.seed_snark_completed_ownership(&[(10, 11), (4, 3)])
                .await,
            Err(SnarkOwnershipSeedError::InvalidRange { from: 4, to: 3 })
        );
        assert_eq!(
            map.admit_snark_job(create_test_batch_envelope(10)).await,
            SnarkJobAdmission::Inserted
        );

        assert_eq!(
            map.seed_snark_completed_ownership(&[(20, 21), (10, 10)])
                .await,
            Err(SnarkOwnershipSeedError::ActiveJob {
                from: 10,
                to: 10,
                batch_number: 10,
            })
        );
        assert_eq!(
            map.admit_snark_job(create_test_batch_envelope(20)).await,
            SnarkJobAdmission::Inserted
        );
        assert!(map.completed_ownership_ranges_for_test().await.is_empty());
    }

    // SYSCOIN: A blocked live admission releases both ownership and queue guards. Recovery seed
    // and exact-token completion can therefore proceed, claim the head, and wake the waiter.
    #[tokio::test]
    async fn capacity_wait_never_holds_completed_ownership() {
        let map = Arc::new(ProverJobMap::new(
            Duration::from_secs(60),
            1,
            ProverStage::Snark,
        ));
        map.add_job(create_test_batch_envelope(1)).await;
        map.add_job(create_test_batch_envelope(2)).await;
        let picked = map
            .pick_job(Duration::ZERO, "snark-owner", |_| true)
            .await
            .expect("head must be leasable");
        let submission = map
            .begin_submission(1, 1, &picked.lease_token)
            .await
            .expect("head capability must enter submission");

        let waiting_map = Arc::clone(&map);
        let waiting = tokio::spawn(async move {
            waiting_map
                .admit_snark_job(create_test_batch_envelope(3))
                .await
        });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());

        tokio::time::timeout(
            Duration::from_secs(1),
            map.seed_snark_completed_ownership(&[(10, 10)]),
        )
        .await
        .expect("capacity waiter retained the completed-ownership lock")
        .expect("disjoint recovery ownership must seed");
        assert!(matches!(
            submission
                .complete_with_snark_ownership(ProverType::Real, "snark-owner",)
                .await,
            SnarkOwnershipCompletion::Completed(_)
        ));
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), waiting)
                .await
                .expect("capacity waiter did not wake")
                .expect("capacity waiter panicked"),
            SnarkJobAdmission::Inserted
        );
        assert_eq!(
            map.admit_snark_job(create_test_batch_envelope(1)).await,
            SnarkJobAdmission::AlreadyOwned
        );
    }

    async fn complete_command_owned_range(
        map: &Arc<ProverJobMap<Vec<u8>>>,
        batch_from: u64,
        batch_to: u64,
        lease_token: &str,
    ) {
        let submission = map
            .begin_submission(batch_from, batch_to, lease_token)
            .await
            .expect("planned range capability must enter submission");
        assert!(matches!(
            submission
                .complete_with_snark_ownership(ProverType::Real, "startup-recovery-test",)
                .await,
            SnarkOwnershipCompletion::Completed(_)
        ));
    }

    // SYSCOIN: Loading and Draining are strict numeric fences. Missing head input, retry release,
    // and later live work cannot advance the plan; only exact completed ownership can do so.
    #[tokio::test]
    async fn startup_recovery_drains_exact_head_before_later_or_live_work() {
        let map = Arc::new(ProverJobMap::new(Duration::ZERO, 100, ProverStage::Snark));
        map.install_startup_recovery_plan(
            StartupRecoveryPlan::build(0, 4, &[], 2, 100, false).unwrap(),
        )
        .await
        .unwrap();

        for batch_number in [1, 3, 4, 5, 6] {
            map.add_job(create_test_batch_envelope(batch_number)).await;
        }
        assert!(matches!(
            map.pick_ready_snark_jobs(2, 2, Duration::ZERO, "missing-head", |_| true)
                .await,
            SnarkJobPick::Waiting(SnarkReadinessWait {
                eligible_fris: 1,
                ..
            })
        ));

        map.add_job(create_test_batch_envelope(2)).await;
        let first = map
            .pick_ready_snark_jobs(2, 2, Duration::ZERO, "first-head", |_| true)
            .await;
        let SnarkJobPick::Assigned { jobs, lease_token } = first else {
            panic!("fully loaded first recovery head must be assigned")
        };
        assert_eq!(
            jobs.iter()
                .map(|(job, _)| job.batch_number)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        let submission = map.begin_submission(1, 2, &lease_token).await.unwrap();
        submission.release_for_retry().await;

        let retry = map
            .pick_ready_snark_jobs(2, 2, Duration::ZERO, "first-head-retry", |_| true)
            .await;
        let SnarkJobPick::Assigned { jobs, lease_token } = retry else {
            panic!("retry release must retain the same recovery head")
        };
        assert_eq!(jobs[0].0.batch_number, 1);
        assert_eq!(jobs[1].0.batch_number, 2);
        complete_command_owned_range(&map, 1, 2, &lease_token).await;

        map.finish_startup_loading().await.unwrap();
        let second = map
            .pick_ready_snark_jobs(2, 2, Duration::ZERO, "second-head", |_| true)
            .await;
        let SnarkJobPick::Assigned { jobs, lease_token } = second else {
            panic!("second recovery head must precede queued live work")
        };
        assert_eq!(
            jobs.iter()
                .map(|(job, _)| job.batch_number)
                .collect::<Vec<_>>(),
            vec![3, 4]
        );
        complete_command_owned_range(&map, 3, 4, &lease_token).await;

        let live = map
            .pick_ready_snark_jobs(2, 2, Duration::ZERO, "live", |_| true)
            .await;
        let SnarkJobPick::Assigned { jobs, .. } = live else {
            panic!("live work must become visible after the draining head completes")
        };
        assert_eq!(
            jobs.iter()
                .map(|(job, _)| job.batch_number)
                .collect::<Vec<_>>(),
            vec![5, 6]
        );
    }

    // SYSCOIN: Startup prefix refinement keeps both sides real-wrapper viable, but a durable
    // journal-ownership hole remains an immutable boundary that cannot rescue an interior 2+1.
    #[tokio::test]
    async fn startup_recovery_byte_prefix_leaves_two_and_rejects_journal_hole_singleton() {
        let map = ProverJobMap::new(Duration::from_secs(60), 100, ProverStage::Snark);
        map.install_startup_recovery_plan(
            StartupRecoveryPlan::build(0, 5, &[], 5, 100, false).unwrap(),
        )
        .await
        .unwrap();
        for batch_number in 1..=5 {
            map.add_job(create_test_batch_envelope(batch_number)).await;
        }
        let mut inspected = 0;
        let pick = map
            .pick_ready_snark_jobs_with_limits(
                5,
                5,
                Duration::ZERO,
                MAX_JOURNAL_RECORD_BYTES,
                "prefix",
                |_| {
                    inspected += 1;
                    if inspected <= 4 {
                        SnarkJobEligibility::Eligible
                    } else {
                        SnarkJobEligibility::ResponseCapacityExceeded {
                            required_bytes: 11,
                            max_bytes: 10,
                        }
                    }
                },
            )
            .await;
        let SnarkJobPick::Assigned { jobs, .. } = pick else {
            panic!("four fitting FRIs out of five must refine to a three-FRI prefix")
        };
        assert_eq!(
            jobs.iter()
                .map(|(job, _)| job.batch_number)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );

        let journal_hole = ProverJobMap::new(Duration::from_secs(60), 100, ProverStage::Snark);
        journal_hole
            .seed_snark_completed_ownership(&[(4, 5)])
            .await
            .unwrap();
        journal_hole
            .install_startup_recovery_plan(
                StartupRecoveryPlan::build(0, 5, &[(4, 5)], 3, 100, false).unwrap(),
            )
            .await
            .unwrap();
        for batch_number in 1..=3 {
            journal_hole
                .add_job(create_test_batch_envelope(batch_number))
                .await;
        }
        let mut inspected = 0;
        let pick = journal_hole
            .pick_ready_snark_jobs_with_limits(
                3,
                3,
                Duration::ZERO,
                MAX_JOURNAL_RECORD_BYTES,
                "journal-hole-singleton",
                |_| {
                    inspected += 1;
                    if inspected <= 2 {
                        SnarkJobEligibility::Eligible
                    } else {
                        SnarkJobEligibility::ResponseCapacityExceeded {
                            required_bytes: 11,
                            max_bytes: 10,
                        }
                    }
                },
            )
            .await;
        assert!(matches!(
            pick,
            SnarkJobPick::Unwrappable {
                batch_from: 1,
                batch_to: 3,
                fittable_fris: 2,
            }
        ));
    }

    // SYSCOIN: Do not classify a repairable boundary as fatal merely because bounded startup
    // rehydration has not admitted the first FRI from the adjacent planned range yet.
    #[tokio::test]
    async fn startup_singleton_repartition_waits_for_nonresident_adjacent_fri() {
        let map = ProverJobMap::new(Duration::from_secs(60), 100, ProverStage::Snark);
        map.install_startup_recovery_plan(
            StartupRecoveryPlan::build(0, 5, &[], 3, 100, false).unwrap(),
        )
        .await
        .unwrap();
        for batch_number in 1..=3 {
            map.add_job(create_test_batch_envelope(batch_number)).await;
        }

        let mut inspected = 0;
        let waiting = map
            .pick_ready_snark_jobs_with_limits(
                3,
                3,
                Duration::ZERO,
                MAX_JOURNAL_RECORD_BYTES,
                "nonresident-successor",
                |_| {
                    inspected += 1;
                    if inspected <= 2 {
                        SnarkJobEligibility::Eligible
                    } else {
                        SnarkJobEligibility::ResponseCapacityExceeded {
                            required_bytes: 11,
                            max_bytes: 10,
                        }
                    }
                },
            )
            .await;
        assert!(matches!(
            waiting,
            SnarkJobPick::Waiting(SnarkReadinessWait {
                eligible_fris: 2,
                ..
            })
        ));

        map.add_job(create_test_batch_envelope(4)).await;
        let mut inspected = 0;
        let repaired = map
            .pick_ready_snark_jobs_with_limits(
                3,
                3,
                Duration::ZERO,
                MAX_JOURNAL_RECORD_BYTES,
                "resident-successor",
                |_| {
                    inspected += 1;
                    if inspected <= 2 {
                        SnarkJobEligibility::Eligible
                    } else {
                        SnarkJobEligibility::ResponseCapacityExceeded {
                            required_bytes: 11,
                            max_bytes: 10,
                        }
                    }
                },
            )
            .await;
        let SnarkJobPick::Assigned { jobs, .. } = repaired else {
            panic!("resident compatible successor must repair the boundary")
        };
        assert_eq!(
            jobs.iter()
                .map(|(job, _)| job.batch_number)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    // SYSCOIN: Runtime singleton repair uses the bounded map's five-job capacity, not only the
    // 100-FRI wrapper cap. The repaired head must remain loadable after two short byte prefixes.
    #[tokio::test]
    async fn startup_repartition_never_exceeds_resident_map_capacity() {
        let map = Arc::new(ProverJobMap::new(
            Duration::from_secs(60),
            4,
            ProverStage::Snark,
        ));
        map.install_startup_recovery_plan(
            StartupRecoveryPlan::build(0, 10, &[], 100, 4, false).unwrap(),
        )
        .await
        .unwrap();
        for batch_number in 1..=5 {
            map.add_job(create_test_batch_envelope(batch_number)).await;
        }

        for (expected_from, newly_admitted) in [(1, None), (3, Some(6))] {
            if let Some(batch_number) = newly_admitted {
                map.add_job(create_test_batch_envelope(batch_number)).await;
            }
            let mut inspected = 0;
            let pick = map
                .pick_ready_snark_jobs_with_limits(
                    100,
                    100,
                    Duration::ZERO,
                    MAX_JOURNAL_RECORD_BYTES,
                    "two-fri-map-capacity-regression",
                    |_| {
                        inspected += 1;
                        if inspected <= 2 {
                            SnarkJobEligibility::Eligible
                        } else {
                            SnarkJobEligibility::ResponseCapacityExceeded {
                                required_bytes: 11,
                                max_bytes: 10,
                            }
                        }
                    },
                )
                .await;
            let SnarkJobPick::Assigned { jobs, lease_token } = pick else {
                panic!("two-FRI recovery prefix {expected_from} must remain assignable")
            };
            assert_eq!(
                jobs.iter()
                    .map(|(job, _)| job.batch_number)
                    .collect::<Vec<_>>(),
                vec![expected_from, expected_from + 1]
            );
            complete_command_owned_range(&map, expected_from, expected_from + 1, &lease_token)
                .await;
        }

        let repaired_head = map
            .pick_ready_snark_jobs(100, 100, Duration::ZERO, "bounded-repaired-head", |_| true)
            .await;
        let SnarkJobPick::Assigned { jobs, .. } = repaired_head else {
            panic!("the repaired head must fit completely in the resident map")
        };
        assert_eq!(
            jobs.iter()
                .map(|(job, _)| job.batch_number)
                .collect::<Vec<_>>(),
            vec![5, 6]
        );
    }

    // SYSCOIN: The initial 101-FRI plan is 99+2 at the count cap. If runtime response bytes admit
    // only two FRIs at a time, repeatedly completing those prefixes must move the interior odd batch
    // into the next compatible range without a duplicate or skip; only the absolute tip may wait.
    #[tokio::test]
    async fn startup_99_plus_2_repartitions_across_repeated_two_fri_prefixes() {
        let map = Arc::new(ProverJobMap::new(
            Duration::from_secs(60),
            256,
            ProverStage::Snark,
        ));
        map.install_startup_recovery_plan(
            StartupRecoveryPlan::build(0, 101, &[], 100, 256, false).unwrap(),
        )
        .await
        .unwrap();
        for batch_number in 1..=101 {
            map.add_job(create_test_batch_envelope(batch_number)).await;
        }
        map.finish_startup_loading().await.unwrap();

        let mut completed_batches = Vec::new();
        for expected_from in (1..=99).step_by(2) {
            let mut inspected = 0;
            let pick = map
                .pick_ready_snark_jobs_with_limits(
                    100,
                    100,
                    Duration::ZERO,
                    MAX_JOURNAL_RECORD_BYTES,
                    "two-fri-runtime-cap",
                    |_| {
                        inspected += 1;
                        if inspected <= 2 {
                            SnarkJobEligibility::Eligible
                        } else {
                            SnarkJobEligibility::ResponseCapacityExceeded {
                                required_bytes: 11,
                                max_bytes: 10,
                            }
                        }
                    },
                )
                .await;
            let SnarkJobPick::Assigned { jobs, lease_token } = pick else {
                panic!(
                    "two-FRI prefix {expected_from}-{} must remain assignable",
                    expected_from + 1
                )
            };
            let picked: Vec<_> = jobs.iter().map(|(job, _)| job.batch_number).collect();
            assert_eq!(picked, vec![expected_from, expected_from + 1]);
            completed_batches.extend(picked);
            complete_command_owned_range(&map, expected_from, expected_from + 1, &lease_token)
                .await;
        }
        assert_eq!(completed_batches, (1..=100).collect::<Vec<_>>());
        assert_eq!(
            map.status()
                .await
                .iter()
                .map(|job| job.fri_job.batch_number)
                .collect::<Vec<_>>(),
            vec![101]
        );
        assert!(matches!(
            map.pick_ready_snark_jobs(100, 100, Duration::ZERO, "absolute-tip", |_| true)
                .await,
            SnarkJobPick::Waiting(SnarkReadinessWait {
                eligible_fris: 1,
                ..
            })
        ));

        map.add_job(create_test_batch_envelope(102)).await;
        let promoted = map
            .pick_ready_snark_jobs(100, 100, Duration::ZERO, "promoted-tip", |_| true)
            .await;
        let SnarkJobPick::Assigned { jobs, lease_token } = promoted else {
            panic!("the absolute singleton must pair with its first live successor")
        };
        assert_eq!(
            jobs.iter()
                .map(|(job, _)| job.batch_number)
                .collect::<Vec<_>>(),
            vec![101, 102]
        );
        complete_command_owned_range(&map, 101, 102, &lease_token).await;
        assert_eq!(
            map.completed_ownership_ranges_for_test().await,
            vec![(1, 102)]
        );
        assert!(map.status().await.is_empty());
    }

    // SYSCOIN: The absolute startup tip is the sole legal real singleton. It remains fenced and
    // unleased until the first contiguous live admission atomically promotes it to a pair.
    #[tokio::test]
    async fn startup_tip_singleton_waits_then_promotes_on_contiguous_admission() {
        let map = Arc::new(ProverJobMap::new(
            Duration::from_secs(60),
            100,
            ProverStage::Snark,
        ));
        map.install_startup_recovery_plan(
            StartupRecoveryPlan::build(0, 3, &[], 3, 100, false).unwrap(),
        )
        .await
        .unwrap();
        for batch_number in 1..=3 {
            map.add_job(create_test_batch_envelope(batch_number)).await;
        }
        let mut inspected = 0;
        let pick = map
            .pick_ready_snark_jobs_with_limits(
                3,
                3,
                Duration::ZERO,
                MAX_JOURNAL_RECORD_BYTES,
                "tip-prefix",
                |_| {
                    inspected += 1;
                    if inspected <= 2 {
                        SnarkJobEligibility::Eligible
                    } else {
                        SnarkJobEligibility::ResponseCapacityExceeded {
                            required_bytes: 11,
                            max_bytes: 10,
                        }
                    }
                },
            )
            .await;
        let SnarkJobPick::Assigned { jobs, lease_token } = pick else {
            panic!("absolute-tip 2+1 may lease the pair")
        };
        assert_eq!(jobs.len(), 2);
        complete_command_owned_range(&map, 1, 2, &lease_token).await;
        map.finish_startup_loading().await.unwrap();

        assert!(matches!(
            map.pick_ready_snark_jobs(3, 3, Duration::ZERO, "tip-waits", |_| true)
                .await,
            SnarkJobPick::Waiting(SnarkReadinessWait {
                eligible_fris: 1,
                ..
            })
        ));
        map.add_job(create_test_batch_envelope(4)).await;
        let promoted = map
            .pick_ready_snark_jobs(3, 3, Duration::ZERO, "tip-promoted", |_| true)
            .await;
        let SnarkJobPick::Assigned { jobs, .. } = promoted else {
            panic!("contiguous live admission must promote the deferred tip")
        };
        assert_eq!(
            jobs.iter()
                .map(|(job, _)| job.batch_number)
                .collect::<Vec<_>>(),
            vec![3, 4]
        );
    }

    #[tokio::test]
    async fn startup_plan_rejects_completed_deferred_tip_ownership() {
        let map = ProverJobMap::<Vec<u8>>::new(Duration::from_secs(60), 100, ProverStage::Snark);
        map.seed_snark_completed_ownership(&[(5, 5)]).await.unwrap();
        assert_eq!(
            map.install_startup_recovery_plan(
                StartupRecoveryPlan::build(4, 5, &[], 100, 100, false).unwrap(),
            )
            .await,
            Err(StartupRecoveryBoundaryError::AlreadyOwned(5))
        );
    }

    #[tokio::test]
    #[should_panic(expected = "conflicting same-number prover job metadata")]
    async fn conflicting_same_number_job_fails_closed() {
        let map = ProverJobMap::new(Duration::from_secs(60), 100, ProverStage::Snark);
        map.add_job(create_test_batch_envelope(1)).await;

        let mut conflicting = create_test_batch_envelope(1);
        conflicting.batch.tx_count += 1;
        map.add_job(conflicting).await;
    }

    #[tokio::test]
    async fn test_pick_job() {
        let map = ProverJobMap::new(Duration::from_secs(60), 100, ProverStage::Fri);

        map.add_job(create_test_batch_envelope(1)).await;
        map.add_job(create_test_batch_envelope(2)).await;

        let job = map.pick_job(Duration::ZERO, "prover-1", |_| true).await;
        assert!(job.is_some());
        let leased_job = job.unwrap();
        assert_eq!(leased_job.job.batch_number, 1);

        // Job 1 is now assigned, should pick job 2
        let job = map.pick_job(Duration::ZERO, "prover-2", |_| true).await;
        assert!(job.is_some());
        let leased_job = job.unwrap();
        assert_eq!(leased_job.job.batch_number, 2);

        // All jobs assigned, should return None
        let job = map.pick_job(Duration::ZERO, "prover-3", |_| true).await;
        assert!(job.is_none());
    }

    #[tokio::test]
    async fn test_pick_job_with_canonical_proving_version_filter() {
        let map = ProverJobMap::new(Duration::from_secs(60), 100, ProverStage::Fri);

        map.add_job(create_test_batch_envelope(1)).await;
        map.add_job(create_test_batch_envelope_with_protocol_version(
            2,
            ProtocolSemanticVersion::new(0, 32, 0),
        ))
        .await;

        let job = map
            .pick_job(Duration::ZERO, "prover-v8", |job| {
                job.metadata.proving_version == ProvingVersion::V8
            })
            .await;

        assert!(job.is_some());
        let leased_job = job.unwrap();
        assert_eq!(leased_job.job.batch_number, 1);
        assert_eq!(leased_job.job.vk_hash, ProvingVersion::V8.vk_hash());

        let status = map.status().await;
        assert_eq!(status[0].fri_job.batch_number, 1);
        assert_eq!(
            status[0].assigned_to_prover_id,
            Some("prover-v8".to_string())
        );
        assert_eq!(status[1].fri_job.batch_number, 2);
        assert_eq!(status[1].assigned_to_prover_id, None);
    }

    #[tokio::test]
    async fn test_pick_job_with_timeout() {
        let map = ProverJobMap::new(Duration::from_millis(100), 100, ProverStage::Fri);

        map.add_job(create_test_batch_envelope(1)).await;

        let job = map.pick_job(Duration::ZERO, "prover-1", |_| true).await;
        assert!(job.is_some());

        // Try to pick again immediately - should return None (still assigned)
        let job = map.pick_job(Duration::ZERO, "prover-2", |_| true).await;
        assert!(job.is_none());

        // Wait for timeout
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Should be able to pick again after timeout
        let job = map.pick_job(Duration::ZERO, "prover-2", |_| true).await;
        assert!(job.is_some());
        let leased_job = job.unwrap();
        assert_eq!(leased_job.job.batch_number, 1);
    }

    #[tokio::test]
    async fn test_pick_multiple_jobs() {
        let map = ProverJobMap::new(Duration::from_secs(60), 100, ProverStage::Snark);

        map.add_job(create_test_batch_envelope(1)).await;
        map.add_job(create_test_batch_envelope(2)).await;
        map.add_job(create_test_batch_envelope(3)).await;

        let jobs = map
            .pick_jobs_while_with_limit(2, "prover-1", |_| true)
            .await;
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].0.batch_number, 1);
        assert_eq!(jobs[1].0.batch_number, 2);
    }

    #[tokio::test]
    async fn test_pick_multiple_jobs_with_gap() {
        let map = ProverJobMap::new(Duration::from_secs(60), 100, ProverStage::Snark);

        map.add_job(create_test_batch_envelope(1)).await;
        map.add_job(create_test_batch_envelope(3)).await; // Gap: no batch 2

        // Should only pick batch 1, not 3 (due to gap)
        let jobs = map
            .pick_jobs_while_with_limit(5, "prover-1", |_| true)
            .await;
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].0.batch_number, 1);
    }

    #[tokio::test]
    async fn test_snark_pick_aggregates_upgrade_metadata_with_same_version_neighbors() {
        let map = ProverJobMap::new(Duration::from_secs(60), 100, ProverStage::Snark);

        map.add_job(create_test_batch_envelope(1)).await;
        map.add_job(create_test_batch_envelope_with_upgrade(
            2,
            Some(B256::from([2; 32])),
        ))
        .await;
        map.add_job(create_test_batch_envelope(3)).await;

        let jobs = map
            .pick_jobs_while_with_limit(5, "prover-1", |_| true)
            .await;
        assert_eq!(
            jobs.iter()
                .map(|(job, _)| job.batch_number)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[tokio::test]
    async fn real_snark_aged_singleton_waits_and_remains_visible() {
        let map = ProverJobMap::new(Duration::from_secs(60), 100, ProverStage::Snark);
        map.add_job_with_age(create_test_batch_envelope(1), Duration::from_secs(3601))
            .await;

        let pick = map
            .pick_ready_snark_jobs(100, 100, Duration::from_secs(3600), "prover-1", |_| true)
            .await;
        let SnarkJobPick::Waiting(wait) = pick else {
            panic!("an aged singleton must still wait for a second compatible proof")
        };
        assert_eq!(wait.eligible_fris, 1);
        assert!(wait.oldest_eligible_age >= Duration::from_secs(3600));

        let status = map.status().await;
        assert_eq!(status.len(), 1);
        assert_eq!(status[0].fri_job.batch_number, 1);
        assert_eq!(status[0].assigned_to_prover_id, None);
    }

    #[tokio::test]
    async fn real_snark_target_count_is_assigned() {
        let map = ProverJobMap::new(Duration::from_secs(60), 100, ProverStage::Snark);
        map.add_job(create_test_batch_envelope(1)).await;
        map.add_job(create_test_batch_envelope_with_upgrade(
            2,
            Some(B256::from([2; 32])),
        ))
        .await;
        map.add_job(create_test_batch_envelope(3)).await;

        let pick = map
            .pick_ready_snark_jobs(100, 3, Duration::from_secs(3600), "prover-1", |_| true)
            .await;
        let SnarkJobPick::Assigned { jobs, .. } = pick else {
            panic!("target-sized range must be assigned")
        };
        assert_eq!(
            jobs.iter()
                .map(|(job, _)| job.batch_number)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[tokio::test]
    async fn journal_capacity_short_prefix_is_immediately_ready_at_exact_limit() {
        let map = ProverJobMap::new(Duration::from_secs(60), 100, ProverStage::Snark);
        for batch_number in 1..=3 {
            map.add_job(create_test_batch_envelope(batch_number)).await;
        }
        let pair_limit = {
            let jobs = map.lock_jobs_for_test().await;
            let pair_json_bytes: usize = jobs
                .range(1..=2)
                .map(|(_, entry)| entry.metadata.durable_snark_batch_json_bytes)
                .sum();
            durable_snark_record_json_upper_bound(1, 2, 2, pair_json_bytes).unwrap()
        };

        // SYSCOIN: The first two fit exactly, while the third exceeds this synthetic cap. The
        // persistable prefix must bypass target/age waiting and receive the only lease.
        let pick = map
            .pick_ready_snark_jobs_with_journal_limit(
                100,
                3,
                Duration::from_secs(3600),
                pair_limit,
                "capacity-splitter",
                |_| true,
            )
            .await;
        let SnarkJobPick::Assigned { jobs, .. } = pick else {
            panic!("an exact-cap persistable prefix must be assigned immediately")
        };
        assert_eq!(
            jobs.iter()
                .map(|(job, _)| job.batch_number)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[tokio::test]
    async fn impossible_two_fri_journal_is_fatal_without_creating_a_lease() {
        let map = ProverJobMap::new(Duration::from_secs(60), 100, ProverStage::Snark);
        for batch_number in 1..=2 {
            map.add_job(create_test_batch_envelope(batch_number)).await;
        }
        let pair_bytes = {
            let jobs = map.lock_jobs_for_test().await;
            let batch_json_bytes: usize = jobs
                .values()
                .map(|entry| entry.metadata.durable_snark_batch_json_bytes)
                .sum();
            durable_snark_record_json_upper_bound(1, 2, 2, batch_json_bytes).unwrap()
        };
        let pick = map
            .pick_ready_snark_jobs_with_journal_limit(
                100,
                100,
                Duration::ZERO,
                pair_bytes - 1,
                "impossible-wrapper",
                |_| true,
            )
            .await;
        assert!(matches!(
            pick,
            SnarkJobPick::Unpersistable {
                batch_from: 1,
                blocked_at: 2,
                ..
            }
        ));
        assert!(
            map.status()
                .await
                .iter()
                .all(|job| job.assigned_to_prover_id.is_none()),
            "an impossible aggregate must fail before any lease is created"
        );
        assert!(
            map.lock_jobs_for_test()
                .await
                .values()
                .all(|entry| entry.metadata.current_attempt == 0),
            "deterministic oversize must not refresh or increment an assignment"
        );
    }

    #[tokio::test]
    async fn hundred_large_message_batches_split_into_contiguous_persistable_prefixes() {
        let mut representative = create_test_batch_envelope(1);
        representative.batch.messages = vec![vec![u8::MAX; 2_100_000]];
        let representative_json_bytes =
            durable_snark_batch_json_bytes(&representative.batch).unwrap();

        let map = ProverJobMap::new(Duration::from_secs(60), 256, ProverStage::Snark);
        for batch_number in 1..=100 {
            map.add_job(create_test_batch_envelope(batch_number)).await;
        }
        {
            // SYSCOIN: Use the exactly measured 2.1-MB compact-message contribution for every
            // light queue fixture, avoiding 210 MB of duplicate test allocation while preserving
            // the production V2 byte-admission calculation.
            let mut jobs = map.lock_jobs_for_test().await;
            for entry in jobs.values_mut() {
                entry.metadata.durable_snark_batch_json_bytes = representative_json_bytes;
            }
        }

        let first = map
            .pick_ready_snark_jobs(
                100,
                100,
                Duration::from_secs(3600),
                "large-prefix-1",
                |_| true,
            )
            .await;
        let SnarkJobPick::Assigned { jobs: first, .. } = first else {
            panic!("journal cap must split and release the first large prefix")
        };
        assert!((2..100).contains(&first.len()));
        let first_to = first.last().unwrap().0.batch_number;

        let second = map
            .pick_ready_snark_jobs(100, 100, Duration::ZERO, "large-prefix-2", |_| true)
            .await;
        let SnarkJobPick::Assigned { jobs: second, .. } = second else {
            panic!("the remaining large contiguous prefix must stay serviceable")
        };
        assert_eq!(second.first().unwrap().0.batch_number, first_to + 1);
        assert_eq!(second.last().unwrap().0.batch_number, 100);
    }

    // SYSCOIN: The production range default must retain a complete next 100-FRI aggregate while
    // the separate CPU SNARK worker holds the preceding 100-FRI lease. Otherwise the SNARK queue
    // backpressures all three resident FRI GPUs for most of a long combine/wrap job.
    #[tokio::test]
    async fn range_256_buffers_two_full_hundred_fri_snark_ranges() {
        let map = ProverJobMap::new(Duration::from_secs(60), 256, ProverStage::Snark);
        for batch_number in 1..=100 {
            map.add_job(create_test_batch_envelope(batch_number)).await;
        }

        let first_pick = map
            .pick_ready_snark_jobs(100, 100, Duration::from_secs(3600), "cpu-snark-1", |_| true)
            .await;
        let SnarkJobPick::Assigned {
            jobs: first_jobs, ..
        } = first_pick
        else {
            panic!("the first 100-FRI range must be assigned")
        };
        assert_eq!(first_jobs.len(), 100);

        tokio::time::timeout(Duration::from_secs(1), async {
            for batch_number in 101..=200 {
                map.add_job(create_test_batch_envelope(batch_number)).await;
            }
        })
        .await
        .expect("the next complete 100-FRI aggregate must fit behind the active lease");

        let second_pick = map
            .pick_ready_snark_jobs(100, 100, Duration::from_secs(3600), "cpu-snark-2", |_| true)
            .await;
        let SnarkJobPick::Assigned {
            jobs: second_jobs, ..
        } = second_pick
        else {
            panic!("the buffered second 100-FRI range must be ready")
        };
        assert_eq!(second_jobs.len(), 100);
        assert_eq!(second_jobs.first().unwrap().0.batch_number, 101);
        assert_eq!(second_jobs.last().unwrap().0.batch_number, 200);
    }

    #[tokio::test]
    async fn interop_bundle_releases_two_proof_range_before_target_or_age() {
        let map = ProverJobMap::new(Duration::from_secs(60), 100, ProverStage::Snark);
        map.add_job(create_test_batch_envelope(1)).await;
        let mut interop_batch = create_test_batch_envelope(2);
        mark_test_batch_as_interop_bundle(&mut interop_batch);
        map.add_job(interop_batch).await;

        let pick = map
            .pick_ready_snark_jobs(100, 100, Duration::from_secs(3600), "prover-1", |_| true)
            .await;
        let SnarkJobPick::Assigned { jobs, .. } = pick else {
            panic!("a two-proof range carrying interop must bypass the target/age delay")
        };
        assert_eq!(
            jobs.iter()
                .map(|(job, _)| job.batch_number)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[tokio::test]
    async fn interop_bundle_singleton_still_waits() {
        let map = ProverJobMap::new(Duration::from_secs(60), 100, ProverStage::Snark);
        let mut interop_batch = create_test_batch_envelope(1);
        mark_test_batch_as_interop_bundle(&mut interop_batch);
        map.add_job(interop_batch).await;

        let pick = map
            .pick_ready_snark_jobs(100, 100, Duration::ZERO, "prover-1", |_| true)
            .await;
        let SnarkJobPick::Waiting(wait) = pick else {
            panic!("interop priority must not weaken the two-FRI minimum")
        };
        assert_eq!(wait.eligible_fris, 1);
        assert_eq!(map.status().await[0].assigned_to_prover_id, None);
    }

    #[tokio::test]
    async fn spoofed_bundle_prefix_does_not_trigger_interop_priority() {
        let map = ProverJobMap::new(Duration::from_secs(60), 100, ProverStage::Snark);
        map.add_job(create_test_batch_envelope(1)).await;
        let mut spoofed_batch = create_test_batch_envelope(2);
        mark_test_batch_as_interop_bundle(&mut spoofed_batch);
        // A direct caller can submit the same prefix to the messenger, but its caller key cannot
        // equal the InteropCenter system-contract address.
        spoofed_batch.batch.logs[0].key = B256::ZERO;
        map.add_job(spoofed_batch).await;

        let pick = map
            .pick_ready_snark_jobs(100, 100, Duration::from_secs(3600), "prover-1", |_| true)
            .await;
        let SnarkJobPick::Waiting(wait) = pick else {
            panic!("an arbitrary messenger payload must not trigger interop priority")
        };
        assert_eq!(wait.eligible_fris, 2);
        assert!(
            map.status()
                .await
                .iter()
                .all(|job| job.assigned_to_prover_id.is_none())
        );
    }

    #[tokio::test]
    async fn real_snark_oldest_age_releases_two_proof_range() {
        let map = ProverJobMap::new(Duration::from_secs(60), 100, ProverStage::Snark);
        map.add_job_with_age(create_test_batch_envelope(1), Duration::from_secs(3601))
            .await;
        map.add_job(create_test_batch_envelope(2)).await;

        let pick = map
            .pick_ready_snark_jobs(100, 100, Duration::from_secs(3600), "prover-1", |_| true)
            .await;
        let SnarkJobPick::Assigned { jobs, .. } = pick else {
            panic!("aged two-proof range must be assigned")
        };
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].0.batch_number, 1);
        assert_eq!(jobs[1].0.batch_number, 2);
    }

    #[tokio::test]
    async fn real_snark_pick_never_crosses_gap() {
        let gap_map = ProverJobMap::new(Duration::from_secs(60), 100, ProverStage::Snark);
        gap_map.add_job(create_test_batch_envelope(1)).await;
        gap_map.add_job(create_test_batch_envelope(3)).await;

        let gap_pick = gap_map
            .pick_ready_snark_jobs(100, 100, Duration::ZERO, "prover-1", |_| true)
            .await;
        let SnarkJobPick::Waiting(wait) = gap_pick else {
            panic!("a gap must not be crossed to satisfy the two-proof minimum")
        };
        assert_eq!(wait.eligible_fris, 1);
        let status = gap_map.status().await;
        assert!(status.iter().all(|job| job.assigned_to_prover_id.is_none()));
    }

    #[tokio::test]
    async fn concurrent_ready_snark_picks_assign_range_once() {
        let map = std::sync::Arc::new(ProverJobMap::new(
            Duration::from_secs(60),
            100,
            ProverStage::Snark,
        ));
        map.add_job(create_test_batch_envelope(1)).await;
        map.add_job(create_test_batch_envelope(2)).await;

        let first_map = map.clone();
        let second_map = map.clone();
        let (first, second) = tokio::join!(
            async move {
                first_map
                    .pick_ready_snark_jobs(100, 2, Duration::from_secs(3600), "prover-1", |_| true)
                    .await
            },
            async move {
                second_map
                    .pick_ready_snark_jobs(100, 2, Duration::from_secs(3600), "prover-2", |_| true)
                    .await
            }
        );

        let assigned_counts = [first, second]
            .into_iter()
            .map(|pick| match pick {
                SnarkJobPick::Assigned { jobs, .. } => jobs.len(),
                SnarkJobPick::Waiting(_)
                | SnarkJobPick::Unpersistable { .. }
                | SnarkJobPick::UnservableResponse { .. }
                | SnarkJobPick::Unwrappable { .. }
                | SnarkJobPick::Empty => 0,
            })
            .collect::<Vec<_>>();
        assert_eq!(assigned_counts.iter().sum::<usize>(), 2);
        assert_eq!(
            assigned_counts.iter().filter(|&&count| count > 0).count(),
            1
        );
    }

    #[tokio::test]
    async fn ready_snark_assignment_is_reclaimed_after_timeout() {
        let map = ProverJobMap::new(Duration::from_millis(10), 100, ProverStage::Snark);
        map.add_job(create_test_batch_envelope(1)).await;
        map.add_job(create_test_batch_envelope(2)).await;

        let first = map
            .pick_ready_snark_jobs(100, 2, Duration::from_secs(3600), "prover-1", |_| true)
            .await;
        assert!(matches!(first, SnarkJobPick::Assigned { .. }));

        let premature_retry = map
            .pick_ready_snark_jobs(100, 2, Duration::from_secs(3600), "prover-2", |_| true)
            .await;
        assert!(matches!(premature_retry, SnarkJobPick::Empty));

        tokio::time::sleep(Duration::from_millis(20)).await;
        let reclaimed = map
            .pick_ready_snark_jobs(100, 2, Duration::from_secs(3600), "prover-2", |_| true)
            .await;
        let SnarkJobPick::Assigned { jobs, .. } = reclaimed else {
            panic!("timed-out SNARK assignment must be reclaimed")
        };
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].0.batch_number, 1);
        assert_eq!(jobs[1].0.batch_number, 2);
    }

    #[tokio::test]
    async fn exact_snark_assignment_rejects_subrange_without_consuming_jobs() {
        let map = Arc::new(ProverJobMap::new(
            Duration::from_secs(60),
            100,
            ProverStage::Snark,
        ));
        map.add_job(create_test_batch_envelope(1)).await;
        map.add_job(create_test_batch_envelope(2)).await;

        let pick = map
            .pick_ready_snark_jobs(100, 2, Duration::from_secs(3600), "prover-1", |_| true)
            .await;
        let SnarkJobPick::Assigned { lease_token, .. } = pick else {
            panic!("ready SNARK range must be assigned")
        };

        assert_eq!(
            map.begin_submission(1, 1, &lease_token).await.err(),
            Some(BeginSubmissionError::InvalidLease)
        );
        assert!(map.get_job_batch_metadata(1).await.is_some());
        assert!(map.get_job_batch_metadata(2).await.is_some());

        let completed = map
            .begin_submission(1, 2, &lease_token)
            .await
            .expect("the exact assigned range must remain admissible")
            .complete_with_snark_ownership(ProverType::Real, "prover-1")
            .await;
        let SnarkOwnershipCompletion::Completed(completed) = completed else {
            panic!("the exact assigned range must remain completable")
        };
        assert_eq!(completed.len(), 2);
    }

    #[tokio::test]
    async fn test_complete_many_jobs() {
        let map = Arc::new(ProverJobMap::new(
            Duration::from_secs(60),
            100,
            ProverStage::Snark,
        ));

        map.add_job(create_test_batch_envelope(1)).await;
        map.add_job(create_test_batch_envelope(2)).await;
        map.add_job(create_test_batch_envelope(3)).await;

        let result = pick_and_begin_snark(&map, 1, 3, "prover-1").await;
        let result = result
            .complete_with_snark_ownership(ProverType::Real, "prover-1")
            .await;
        let SnarkOwnershipCompletion::Completed(envelopes) = result else {
            panic!("the exact leased range must complete")
        };
        assert_eq!(envelopes.len(), 3);

        // All jobs should be removed
        assert!(map.get_job_batch_metadata(1).await.is_none());
        assert!(map.get_job_batch_metadata(2).await.is_none());
        assert!(map.get_job_batch_metadata(3).await.is_none());
    }

    #[tokio::test]
    async fn test_complete_many_jobs_with_missing() {
        let map = Arc::new(ProverJobMap::new(
            Duration::from_secs(60),
            100,
            ProverStage::Snark,
        ));

        map.add_job(create_test_batch_envelope(1)).await;
        map.add_job(create_test_batch_envelope(2)).await;
        map.add_job(create_test_batch_envelope(3)).await;

        let submission = pick_and_begin_snark(&map, 1, 3, "prover-1").await;
        // SYSCOIN: Inject an impossible partial-loss state after authentic admission to retain
        // the remover's all-or-nothing defense without exposing an unleased completion API.
        map.lock_with_tracking(JobMapMethod::CompleteManyJobs)
            .await
            .remove(&2);
        let result = submission
            .complete_with_snark_ownership(ProverType::Real, "prover-1")
            .await;
        assert!(matches!(result, SnarkOwnershipCompletion::Stale));

        // Original jobs should still be there
        assert!(map.get_job_batch_metadata(1).await.is_some());
        assert!(map.get_job_batch_metadata(3).await.is_some());
    }

    #[tokio::test]
    async fn test_complete_many_jobs_rejects_inverted_range() {
        let map = Arc::new(ProverJobMap::new(
            Duration::from_secs(60),
            100,
            ProverStage::Snark,
        ));

        map.add_job(create_test_batch_envelope(1)).await;
        map.add_job(create_test_batch_envelope(2)).await;

        let picked = map
            .pick_ready_snark_jobs(2, 2, Duration::ZERO, "prover-1", |_| true)
            .await;
        let SnarkJobPick::Assigned { lease_token, .. } = picked else {
            panic!("ready SNARK range must be assigned")
        };

        assert_eq!(
            map.begin_submission(2, 1, &lease_token).await.err(),
            Some(BeginSubmissionError::InvalidRange)
        );
        assert!(map.get_job_batch_metadata(2).await.is_some());

        let result = map
            .begin_submission(1, 2, &lease_token)
            .await
            .expect("the valid assignment must remain admissible")
            .complete_with_snark_ownership(ProverType::Real, "prover-1")
            .await;
        assert!(matches!(result, SnarkOwnershipCompletion::Completed(_)));
    }

    #[tokio::test]
    async fn test_batch_range_limit() {
        use std::sync::Arc;

        let map = Arc::new(ProverJobMap::new(
            Duration::from_secs(60),
            5, // Small range limit
            ProverStage::Fri,
        ));

        // SYSCOIN: A span exactly equal to the configured endpoint difference remains valid.
        for i in 1..=6 {
            map.add_job(create_test_batch_envelope(i)).await;
        }

        // Extending beyond that span must actually block until the head is completed.
        let map_clone = Arc::clone(&map);
        let mut add_task = tokio::spawn(async move {
            map_clone.add_job(create_test_batch_envelope(7)).await;
        });

        // Give it time to hit the limit
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut add_task)
                .await
                .is_err(),
            "live admission must wait while its prospective span exceeds the bound"
        );

        // Complete a job to make space
        pick_begin_and_complete_fake_fri(&map, 1, "prover-1").await;

        // Now the add should succeed
        tokio::time::timeout(Duration::from_millis(500), add_task)
            .await
            .expect("add_job should complete after space is available")
            .expect("task should not panic");

        assert!(map.get_job_batch_metadata(7).await.is_some());
    }

    #[tokio::test]
    async fn test_status() {
        let map = ProverJobMap::new(Duration::from_secs(60), 100, ProverStage::Fri);

        map.add_job(create_test_batch_envelope(1)).await;
        map.add_job(create_test_batch_envelope(2)).await;

        let _ = map.pick_job(Duration::ZERO, "prover-1", |_| true).await;

        let status = map.status().await;
        assert_eq!(status.len(), 2);

        // Batch 1 should be assigned
        assert_eq!(status[0].fri_job.batch_number, 1);
        assert!(status[0].assigned_seconds_ago.is_some());
        assert_eq!(
            status[0].assigned_to_prover_id,
            Some("prover-1".to_string())
        );

        // Batch 2 should be pending
        assert_eq!(status[1].fri_job.batch_number, 2);
        assert!(status[1].assigned_seconds_ago.is_none());
    }

    #[tokio::test]
    async fn opaque_lease_is_atomic_redacted_and_not_bound_to_display_id() {
        let map = Arc::new(ProverJobMap::new(
            Duration::from_secs(60),
            100,
            ProverStage::Fri,
        ));
        map.add_job(create_test_batch_envelope(1)).await;
        let picked = map
            .pick_job(Duration::ZERO, "display-a", |_| true)
            .await
            .expect("job must be leased");

        // SYSCOIN: The monitoring payload may expose a display label, never bearer authority.
        let status_json = serde_json::to_string(&map.status().await).unwrap();
        assert!(!status_json.contains("lease_token"));
        assert!(!status_json.contains(&picked.lease_token));
        assert!(!format!("{picked:?}").contains(&picked.lease_token));
        assert!(!format!("{map:?}").contains(&picked.lease_token));

        assert_eq!(
            map.begin_submission(
                1,
                1,
                "0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            )
            .await
            .unwrap_err(),
            BeginSubmissionError::InvalidLease
        );
        let submission = map
            .begin_submission(1, 1, &picked.lease_token)
            .await
            .expect("exact token must enter submission state");
        assert_eq!(
            map.begin_submission(1, 1, &picked.lease_token)
                .await
                .unwrap_err(),
            BeginSubmissionError::AlreadySubmitting
        );

        // SYSCOIN: The token authorizes completion even when the submitted diagnostic ID differs.
        let completed = submission
            .complete_fake_fri("display-b")
            .await
            .expect("exact token must complete");
        assert_eq!(completed.len(), 1);
        assert!(map.status().await.is_empty());
    }

    #[tokio::test]
    async fn begin_submission_does_not_clone_queued_payload_data() {
        struct CloneCountingData(Arc<std::sync::atomic::AtomicUsize>);

        impl Clone for CloneCountingData {
            fn clone(&self) -> Self {
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Self(Arc::clone(&self.0))
            }
        }

        let clone_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let map = Arc::new(ProverJobMap::new(
            Duration::from_secs(60),
            100,
            ProverStage::Fri,
        ));
        map.add_job(
            create_test_batch_envelope(1).with_data(CloneCountingData(Arc::clone(&clone_count))),
        )
        .await;
        let picked = map
            .pick_job(Duration::ZERO, "display-a", |_| true)
            .await
            .expect("job must be leased");
        // SYSCOIN: Picking necessarily returns T to the prover. Admission must not duplicate it.
        clone_count.store(0, std::sync::atomic::Ordering::SeqCst);

        let submission = map
            .begin_submission(1, 1, &picked.lease_token)
            .await
            .expect("exact token must enter submission state");
        assert_eq!(submission.batch_metadata().count(), 1);
        assert_eq!(
            clone_count.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "submission admission cloned queued prover payload data"
        );
        submission.release().await;
    }

    // SYSCOIN: A large SNARK range snapshots only immutable versions until proof acceptance;
    // full batch logs/messages/signatures remain exclusive to the one-batch FRI verifier.
    #[tokio::test]
    async fn snark_submission_does_not_snapshot_full_batch_metadata() {
        let map = Arc::new(ProverJobMap::new(
            Duration::from_secs(60),
            100,
            ProverStage::Snark,
        ));
        for batch_number in 1..=2 {
            map.add_job(create_test_batch_envelope(batch_number).with_data(FriProof::Fake))
                .await;
        }
        let leased = map
            .pick_ready_snark_jobs(100, 2, Duration::ZERO, "display-a", |_| true)
            .await;
        let SnarkJobPick::Assigned { lease_token, .. } = leased else {
            panic!("two-batch SNARK range must be leased");
        };
        let submission = map
            .begin_submission(1, 2, &lease_token)
            .await
            .expect("exact aggregate token must be admitted");
        assert_eq!(submission.proving_versions().count(), 2);
        assert_eq!(submission.batch_metadata().count(), 0);
        assert!(submission.first_signature_data().is_none());
        submission.release().await;
    }

    #[tokio::test]
    async fn dropped_submission_guard_releases_only_its_exact_state() {
        let map = Arc::new(ProverJobMap::new(
            Duration::from_secs(60),
            100,
            ProverStage::Fri,
        ));
        map.add_job(create_test_batch_envelope(1)).await;
        let picked = map
            .pick_job(Duration::ZERO, "display-a", |_| true)
            .await
            .expect("job must be leased");
        let submission = map
            .begin_submission(1, 1, &picked.lease_token)
            .await
            .expect("exact token must enter submission state");
        drop(submission);

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                match map.begin_submission(1, 1, &picked.lease_token).await {
                    Ok(submission) => {
                        submission.release().await;
                        break;
                    }
                    Err(BeginSubmissionError::AlreadySubmitting) => tokio::task::yield_now().await,
                    Err(err) => panic!("unexpected lease cleanup result: {err:?}"),
                }
            }
        })
        .await
        .expect("RAII cleanup must clear submission-in-progress");
    }

    // SYSCOIN: Removing a completed endpoint must not let a farther batch consume capacity that
    // an in-flight durable handoff may still need to roll back into the live queue.
    #[tokio::test]
    async fn fri_rollback_reservation_fences_endpoint_capacity_until_rollback() {
        let map = Arc::new(ProverJobMap::new(
            Duration::from_secs(60),
            2,
            ProverStage::Fri,
        ));
        map.add_job(create_test_batch_envelope(1)).await;
        map.add_job(create_test_batch_envelope(2)).await;
        let picked = map
            .pick_job(Duration::ZERO, "fri-owner", |_| true)
            .await
            .expect("head must be leasable");
        let reservation = map
            .begin_submission(1, 1, &picked.lease_token)
            .await
            .expect("head capability must enter submission")
            .complete_fri_with_rollback_reservation("fri-owner")
            .await
            .expect("head must transition to a rollback reservation");

        let waiting_map = Arc::clone(&map);
        let waiting = tokio::spawn(async move {
            waiting_map.add_job(create_test_batch_envelope(4)).await;
        });
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(!waiting.is_finished());

        reservation.rollback().await;
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(!waiting.is_finished());
        assert_eq!(
            map.status()
                .await
                .into_iter()
                .map(|job| job.fri_job.batch_number)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );

        let picked = map
            .pick_job(Duration::ZERO, "capacity-test", |_| true)
            .await
            .expect("restored endpoint must remain leasable");
        let reservation = map
            .begin_submission(1, 1, &picked.lease_token)
            .await
            .expect("restored endpoint capability must enter submission")
            .complete_fri_with_rollback_reservation("capacity-test")
            .await
            .expect("restored endpoint must re-enter the real FRI handoff path");
        reservation.commit().await;
        tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("released endpoint must wake capacity waiter")
            .expect("capacity waiter panicked");
        assert_eq!(
            map.status()
                .await
                .into_iter()
                .map(|job| job.fri_job.batch_number)
                .collect::<Vec<_>>(),
            vec![2, 4]
        );
    }

    // SYSCOIN: Once downstream accepts the proof, releasing the exact reservation must wake a
    // waiter whose prospective span is now safe.
    #[tokio::test]
    async fn committed_fri_reservation_releases_endpoint_capacity() {
        let map = Arc::new(ProverJobMap::new(
            Duration::from_secs(60),
            2,
            ProverStage::Fri,
        ));
        map.add_job(create_test_batch_envelope(1)).await;
        map.add_job(create_test_batch_envelope(2)).await;
        let picked = map
            .pick_job(Duration::ZERO, "fri-owner", |_| true)
            .await
            .expect("head must be leasable");
        let reservation = map
            .begin_submission(1, 1, &picked.lease_token)
            .await
            .expect("head capability must enter submission")
            .complete_fri_with_rollback_reservation("fri-owner")
            .await
            .expect("head must transition to a rollback reservation");

        let waiting_map = Arc::clone(&map);
        let waiting = tokio::spawn(async move {
            waiting_map.add_job(create_test_batch_envelope(4)).await;
        });
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(!waiting.is_finished());
        reservation.commit().await;
        tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("reservation release must wake capacity waiter")
            .expect("capacity waiter panicked");
    }

    // SYSCOIN: Pipeline replay of the canonical batch must remain idempotent while a completed
    // proof owns its rollback reservation; otherwise one batch could acquire overlapping jobs.
    #[tokio::test]
    async fn identical_fri_replay_is_ignored_while_rollback_reserved() {
        let map = Arc::new(ProverJobMap::new(
            Duration::from_secs(60),
            2,
            ProverStage::Fri,
        ));
        map.add_job(create_test_batch_envelope(1)).await;
        let picked = map
            .pick_job(Duration::ZERO, "fri-owner", |_| true)
            .await
            .expect("job must be leasable");
        let reservation = map
            .begin_submission(1, 1, &picked.lease_token)
            .await
            .expect("capability must enter submission")
            .complete_fri_with_rollback_reservation("fri-owner")
            .await
            .expect("job must transition to a rollback reservation");

        let replay = map
            .add_job_with_age_inner(create_test_batch_envelope(1), Duration::ZERO, false)
            .await
            .expect("identical replay must not consume capacity");
        assert_eq!(replay, SnarkJobAdmission::Duplicate);
        assert!(map.status().await.is_empty());

        reservation.rollback().await;
        assert_eq!(map.status().await.len(), 1);
    }

    // SYSCOIN: Cancellation of an accepted-proof owner must schedule exact restoration instead
    // of leaking both the job and its endpoint-capacity fence.
    #[tokio::test]
    async fn dropped_fri_reservation_restores_exact_job() {
        let map = Arc::new(ProverJobMap::new(
            Duration::from_secs(60),
            2,
            ProverStage::Fri,
        ));
        map.add_job(create_test_batch_envelope(1)).await;
        let picked = map
            .pick_job(Duration::ZERO, "fri-owner", |_| true)
            .await
            .expect("job must be leasable");
        let reservation = map
            .begin_submission(1, 1, &picked.lease_token)
            .await
            .expect("capability must enter submission")
            .complete_fri_with_rollback_reservation("fri-owner")
            .await
            .expect("job must transition to a rollback reservation");
        drop(reservation);

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if map.status().await.len() == 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dropped reservation must restore its exact job");
        assert!(
            map.pick_job(Duration::ZERO, "new-owner", |_| true)
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn test_get_prover_input() {
        let map = ProverJobMap::new(Duration::from_secs(60), 100, ProverStage::Fri);

        let envelope = create_test_batch_envelope(1);
        map.add_job(envelope).await;

        let result = map.get_prover_input(1).await;
        assert!(result.is_some());
        let (_vk, data) = result.unwrap();
        assert_eq!(data, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn test_double_complete_job() {
        let map = Arc::new(ProverJobMap::new(
            Duration::from_secs(60),
            100,
            ProverStage::Fri,
        ));

        map.add_job(create_test_batch_envelope(1)).await;

        let result1 = pick_begin_and_complete_fake_fri(&map, 1, "prover-1").await;
        assert!(result1.is_some());

        let result2 = pick_begin_and_complete_fake_fri(&map, 1, "prover-1").await;
        assert!(result2.is_none());
    }
}
