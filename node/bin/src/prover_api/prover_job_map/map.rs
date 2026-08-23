use super::models::{
    JobBatchStats, JobEntry, JobMetadata, NonEmptyQueueStatistics, QueueStatistics,
};
use super::tracked_lock::TrackedLockGuard;
use crate::prover_api::fri_job_manager::{FriJob, JobState};
use crate::prover_api::metrics::{JobMapMethod, PROVER_METRICS, ProverStage, ProverType};
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Notify};
use zksync_os_batch_types::batcher_model::{
    BatchMetadata, BatchSignatureData, SignedBatchEnvelope,
};

/// Concurrent map of prover jobs that support FRI and SNARK workflows.
/// Imposes a limit on batch range
/// Keys are batch numbers stored in a BTreeMap for ordered iteration.
/// Values are prover input - concrete types depend on the prover stage
///     (FRI - prover_input (Vec<u32>), SNARK - fri_proof).
///  * add_job - adds a new job (one batch)
///     * blocks if adding this job would exceed max_assigned_batch_range until space is available
///  * pick_job - picks the first job that is either pending or assigned and older than min_age
///     * currently, it iterates over all jobs and picks the first one that meets the criteria
///  * complete_job - marks a job as complete by removing it from the map
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

/// SYSCOIN: Diagnostic state for an aggregate held below its target-or-age threshold.
#[derive(Debug)]
pub struct SnarkReadinessWait {
    pub eligible_fris: usize,
    pub oldest_eligible_age: Duration,
}

/// SYSCOIN: Atomic outcome of readiness inspection and aggregate leasing.
#[derive(Debug)]
pub enum SnarkJobPick<T> {
    Assigned(Vec<(FriJob, T)>),
    Waiting(SnarkReadinessWait),
    Empty,
}

impl<T: Clone> ProverJobMap<T> {
    pub fn new(
        assignment_timeout: Duration,
        max_assigned_batch_range: usize,
        prover_stage: ProverStage,
    ) -> Self {
        Self {
            jobs: Mutex::new(BTreeMap::new()),
            space_available: Notify::new(),
            assignment_timeout,
            max_assigned_batch_range,
            prover_stage,
        }
    }

    /// Adds a pending job to the map.
    /// Awaits if adding this job exceeds `max_assigned_batch_range` until space is available.
    pub async fn add_job(&self, batch_envelope: SignedBatchEnvelope<T>) {
        self.add_job_with_age(batch_envelope, Duration::ZERO).await;
    }

    /// SYSCOIN: Adds a pending job while preserving age reconstructed from durable storage.
    pub async fn add_job_with_age(
        &self,
        batch_envelope: SignedBatchEnvelope<T>,
        existing_age: Duration,
    ) {
        let batch_number = batch_envelope.batch_number();
        let mut jobs = self.lock_with_tracking(JobMapMethod::AddJob).await;

        loop {
            // Startup rehydration intentionally runs before the recreated pipeline is drained,
            // so the same canonical batch can arrive here twice. Treat that second arrival as an
            // idempotent replay: replacing the entry would reset its aggregation clock and,
            // worse, could erase a lease that an external prover already picked up. This check
            // belongs inside the loop because another waiter can insert the batch while this
            // caller is blocked on queue capacity.
            if let Some(existing) = jobs.get_mut(&batch_number) {
                let existing_batch = serde_json::to_vec(&existing.batch_envelope.batch)
                    .expect("canonical batch metadata must serialize");
                let replayed_batch = serde_json::to_vec(&batch_envelope.batch)
                    .expect("replayed batch metadata must serialize");
                // A same-number batch with different authoritative metadata is not an idempotent
                // replay. This invariant is checked after committed-provider validation, so
                // continuing with either value would hide corrupt or contradictory pipeline
                // state. Stop the owning task instead of reporting a successful enqueue.
                assert!(
                    existing_batch == replayed_batch,
                    "conflicting same-number prover job metadata for batch {batch_number} at {:?} stage",
                    self.prover_stage
                );

                let replay_metadata =
                    JobMetadata::new_from_batch_with_age(&batch_envelope, existing_age);
                if replay_metadata.added_at < existing.metadata.added_at {
                    existing.metadata.added_at = replay_metadata.added_at;
                }

                tracing::info!(
                    batch_number,
                    assigned_to_prover_id = ?existing.metadata.assigned_to_prover_id,
                    ?self.prover_stage,
                    "Ignored duplicate prover job replay while preserving existing queue state"
                );
                return;
            }

            // Wait until there's space available (await if batch range limit would be exceeded).
            if !self.is_queue_full(&jobs) {
                break;
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
            // Drop lock before awaiting notification
            drop(jobs);
            notified.await;
            // Re-acquire lock after notification
            jobs = self.lock_with_tracking(JobMapMethod::AddJob).await;
        }

        let entry = JobEntry {
            metadata: JobMetadata::new_from_batch_with_age(&batch_envelope, existing_age),
            batch_envelope,
        };

        jobs.insert(batch_number, entry);

        tracing::info!(
            batch_number,
            queue_statistics = ?self.compute_and_record_statistics(&jobs),
            ?self.prover_stage,
            "Job added"
        );
    }

    /// SYSCOIN: Restores a job that was removed during completion but could not be handed off.
    ///
    /// This intentionally bypasses the range wait in `add_job`: the job already occupied
    /// space in the map, and blocking while trying to undo a failed handoff can strand it.
    pub async fn restore_job(&self, batch_envelope: SignedBatchEnvelope<T>) {
        let batch_number = batch_envelope.batch_number();
        let mut jobs = self.lock_with_tracking(JobMapMethod::AddJob).await;
        let entry = JobEntry {
            metadata: JobMetadata::new_from_batch(&batch_envelope),
            batch_envelope,
        };

        if jobs.insert(batch_number, entry).is_some() {
            tracing::warn!(
                batch_number,
                ?self.prover_stage,
                "Restored job replaced an existing job"
            );
        } else {
            tracing::warn!(
                batch_number,
                ?self.prover_stage,
                "Restored job after failed downstream handoff"
            );
        }
    }

    /// Picks the first job (lowest batch number) that is either:
    /// - Pending and older than min_age (fake provers use non-empty min_age)
    /// - Assigned and timed out
    ///
    /// Returns None if no eligible job is found.
    ///
    /// Used for FRI jobs (one batch == one job)
    pub async fn pick_job<F>(
        &self,
        min_age: Duration,
        prover_id: &str,
        mut predicate: F,
    ) -> Option<(FriJob, T)>
    where
        F: FnMut(&JobEntry<T>) -> bool,
    {
        let now = Instant::now();
        let mut result = self
            .pick_jobs_while_with_limit(1, prover_id, |entry| {
                // min_age is non-zero only for fake provers
                // for real provers this is no-op - that is, we always take the oldest eligible job
                now.duration_since(entry.metadata.added_at) >= min_age && predicate(entry)
            })
            .await;

        result.pop()
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
    pub async fn pick_jobs_while_with_limit<F>(
        &self,
        limit: usize,
        prover_id: &str,
        mut predicate: F,
    ) -> Vec<(FriJob, T)>
    where
        F: FnMut(&JobEntry<T>) -> bool,
    {
        let now = Instant::now();
        let mut jobs = self.lock_with_tracking(JobMapMethod::PickJobsWhile).await;

        let mut selected_jobs = Vec::new();
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

        if selected_jobs.is_empty() {
            return Vec::new();
        }

        let assigned_batch_range = (
            selected_jobs.first().unwrap().batch_number,
            selected_jobs.last().unwrap().batch_number,
        );
        for metadata in &selected_jobs {
            jobs.get_mut(&metadata.batch_number)
                .expect("selected prover job disappeared while holding the queue lock")
                .metadata
                .assign(now, prover_id.to_string(), assigned_batch_range);
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

        selected_jobs
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
            .collect()
    }

    /// SYSCOIN: Atomically inspects, readiness-gates, and assigns the oldest real SNARK range.
    ///
    /// A real range is assigned only once it contains at least two compatible FRI proofs and
    /// either reaches `target_fris`, its oldest proof reaches `max_wait`, or it contains a V32
    /// InteropCenter bundle whose settlement should not wait for the normal amortization window.
    pub async fn pick_ready_snark_jobs<F>(
        &self,
        limit: usize,
        target_fris: usize,
        max_wait: Duration,
        prover_id: &str,
        mut predicate: F,
    ) -> SnarkJobPick<T>
    where
        F: FnMut(&JobEntry<T>) -> bool,
    {
        assert_eq!(self.prover_stage, ProverStage::Snark);
        assert!(limit >= 2);
        assert!((2..=limit).contains(&target_fris));

        let now = Instant::now();
        let mut jobs = self.lock_with_tracking(JobMapMethod::PickJobsWhile).await;
        let mut candidate_jobs = Vec::<JobMetadata>::new();

        for entry in jobs.values() {
            let is_assignable = match entry.metadata.assigned_at {
                None => true,
                Some(assigned_at) => now.duration_since(assigned_at) >= self.assignment_timeout,
            };

            if candidate_jobs.is_empty() {
                if !is_assignable || !predicate(entry) {
                    continue;
                }
                candidate_jobs.push(entry.metadata.clone());
                continue;
            }

            if candidate_jobs.len() >= limit {
                break;
            }

            let last = candidate_jobs.last().unwrap();
            if last.batch_number + 1 != entry.metadata.batch_number {
                break;
            }

            if entry.metadata.proving_version != last.proving_version
                || !is_assignable
                || !predicate(entry)
            {
                break;
            }

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
                || contains_interop_bundle);

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
        for metadata in &candidate_jobs {
            jobs.get_mut(&metadata.batch_number)
                .expect("candidate SNARK job disappeared while holding the queue lock")
                .metadata
                .assign(now, prover_id.to_string(), assigned_batch_range);
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
            "Ready SNARK job assigned",
        );

        SnarkJobPick::Assigned(
            candidate_jobs
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
        )
    }

    pub async fn has_assignable_job<F>(&self, mut predicate: F) -> bool
    where
        F: FnMut(&JobEntry<T>) -> bool,
    {
        let now = Instant::now();
        let jobs = self.lock_with_tracking(JobMapMethod::PickJobsWhile).await;
        jobs.values()
            .any(|entry| self.is_job_eligible(&[], entry, now, 1, &mut predicate))
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
        let is_assignable = match next_job_entry.metadata.assigned_at {
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

    /// Clears the assignment of a job so it becomes immediately available for pick-up,
    /// without waiting for `assignment_timeout` (which is set to many hours for slow
    /// CPU provers). Only applies if the job is still assigned to `prover_id` -
    /// a reassignment to another prover is left untouched.
    pub async fn unassign_job(&self, batch_number: u64, prover_id: &str) {
        let mut jobs = self.lock_with_tracking(JobMapMethod::UnassignJob).await;
        if let Some(entry) = jobs.get_mut(&batch_number)
            && entry.metadata.assigned_to_prover_id.as_deref() == Some(prover_id)
        {
            entry.metadata.unassign();
            tracing::info!(
                batch_number,
                prover_id,
                ?self.prover_stage,
                "Job unassigned after rejected submission - available for immediate pick-up"
            );
        }
    }

    /// If a job is present for a given batch_number, returns the corresponding BatchMetadata
    pub async fn get_job_batch_metadata(&self, batch_number: u64) -> Option<BatchMetadata> {
        let jobs = self
            .lock_with_tracking(JobMapMethod::GetJobBatchMetadata)
            .await;
        jobs.get(&batch_number)
            .map(|entry| entry.batch_envelope.batch.clone())
    }

    // SYSCOIN: Return the signed canonical batch metadata used to validate a submitted FRI proof.
    pub async fn get_job_batch_metadata_and_signature(
        &self,
        batch_number: u64,
    ) -> Option<(BatchMetadata, BatchSignatureData)> {
        let jobs = self
            .lock_with_tracking(JobMapMethod::GetJobBatchMetadata)
            .await;
        jobs.get(&batch_number).map(|entry| {
            (
                entry.batch_envelope.batch.clone(),
                entry.batch_envelope.signature_data.clone(),
            )
        })
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

    /// If a job is present for the given batch_number, returns its proving VK hash.
    pub async fn get_job_proving_vk_hash(&self, batch_number: u64) -> Option<&'static str> {
        let jobs = self.lock_with_tracking(JobMapMethod::GetProverInput).await;
        jobs.get(&batch_number)
            .map(|entry| entry.metadata.proving_version.vk_hash())
    }

    /// Marks a job as complete by removing it from the map.
    /// Notifies inbound jobs waiting in add_job() that space may be available.
    /// Records metrics and logs timing info. Returns the batch envelope if the job existed.
    ///
    /// Used for FRI jobs (one batch == one job)
    pub async fn complete_job(
        &self,
        batch_number: u64,
        prover_type: ProverType,
        prover_id: &str,
    ) -> Option<SignedBatchEnvelope<T>> {
        self.complete_many_jobs(batch_number, batch_number, prover_type, prover_id)
            .await
            .and_then(|mut envelopes| envelopes.pop())
    }

    /// Marks a job as complete by removing it from the map.
    /// Notifies inbound jobs waiting in add_job() that space may be available.
    /// Records metrics and logs timing info. Returns the batch envelope if the job existed.
    ///
    /// Ensures that all completed jobs still exist in the map -
    ///   returns None if any of them were removed (complete before)
    pub async fn complete_many_jobs(
        &self,
        batch_number_from: u64,
        batch_number_to: u64,
        prover_type: ProverType,
        prover_id: &str,
    ) -> Option<Vec<SignedBatchEnvelope<T>>> {
        self.complete_many_jobs_inner(
            batch_number_from,
            batch_number_to,
            prover_type,
            prover_id,
            false,
        )
        .await
    }

    /// SYSCOIN: Completes only the exact range most recently assigned to `prover_id`.
    ///
    /// External SNARK submission uses this boundary so a prover cannot consume a subset of an
    /// assigned aggregate by choosing a larger range that merely happens to exist in the queue.
    pub async fn complete_assigned_many_jobs(
        &self,
        batch_number_from: u64,
        batch_number_to: u64,
        prover_type: ProverType,
        prover_id: &str,
    ) -> Option<Vec<SignedBatchEnvelope<T>>> {
        self.complete_many_jobs_inner(
            batch_number_from,
            batch_number_to,
            prover_type,
            prover_id,
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
        require_exact_assignment: bool,
    ) -> Option<Vec<SignedBatchEnvelope<T>>> {
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
            return None;
        }

        let mut jobs = self
            .lock_with_tracking(JobMapMethod::CompleteManyJobs)
            .await;
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
                return None;
            };
            if require_exact_assignment
                && (entry.metadata.assigned_to_prover_id.as_deref() != Some(prover_id)
                    || entry.metadata.assigned_batch_range
                        != Some((batch_number_from, batch_number_to)))
            {
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
                return None;
            }
        }
        // There is no race condition (TOCTOU) possible here as we hold the mutex lock.
        // All jobs exist - can mark as completed
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
        // Notify once for all completed jobs
        self.space_available.notify_waiters();

        // Record Prometheus metrics
        match &stats.job_with_max_attempts_info {
            // only writing metrics for normal case - the last assigned prover reported result
            Some(assignment_info) if assignment_info.last_assigned_to == prover_id => {
                PROVER_METRICS.prove_time[&(self.prover_stage, prover_type, prover_id.to_string())]
                    // time since last assignment is proving time
                    .observe(assignment_info.time_since_last_assignment);
                if let Some(total_computational_native_used) = stats.total_computational_native_used
                {
                    PROVER_METRICS.computational_native_proven
                        [&(self.prover_stage, prover_type, prover_id.to_string())]
                        .observe(total_computational_native_used);
                    if total_computational_native_used > 0 {
                        PROVER_METRICS.prove_time_per_million_native
                            [&(self.prover_stage, prover_type, prover_id.to_string())]
                            .observe(
                                assignment_info
                                    .time_since_last_assignment
                                    .div_f64(total_computational_native_used as f64 / 1_000_000.0),
                            );
                    }
                }
                if stats.total_txs > 0 {
                    PROVER_METRICS.prove_time_per_tx
                        [&(self.prover_stage, prover_type, prover_id.to_string())]
                        .observe(
                            assignment_info.time_since_last_assignment / stats.total_txs as u32,
                        );
                }
                PROVER_METRICS.proved_after_attempts[&(self.prover_stage, prover_type)]
                    .observe(assignment_info.attempts as f64);
            }
            Some(_) => {
                tracing::info!(
                    ?stats,
                    ?self.prover_stage,
                    "Received a valid proof for a job assigned to another prover - possible timeout. Consider increasing assignment_timeout."
                )
            }
            None => {
                tracing::info!(
                    ?stats,
                    ?self.prover_stage,
                    "Received a valid proof for a job not marked as assigned - possibly assigned before a restart."
                )
            }
        }

        Some(completed.into_iter().map(|e| e.batch_envelope).collect())
    }

    /// Check if the queue is full (range between the oldest and newest batch >= max_assigned_batch_range)
    /// Only used when adding a new job
    fn is_queue_full(&self, jobs: &BTreeMap<u64, JobEntry<T>>) -> bool {
        if let (Some(&min), Some(&max)) = (jobs.keys().next(), jobs.keys().next_back()) {
            max - min >= self.max_assigned_batch_range as u64
        } else {
            false
        }
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
    use crate::prover_api::test_util::mark_test_batch_as_interop_bundle;
    use alloy::primitives::{Address, B256, keccak256};
    use std::time::Duration;
    use zksync_os_batch_types::{PendingBatchInfo, batcher_model::BatchForSigning};
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
            ProtocolSemanticVersion::canonical_genesis_version(),
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

    #[tokio::test]
    async fn test_add_and_complete_job() {
        let map = ProverJobMap::new(Duration::from_secs(60), 100, ProverStage::Fri);

        let envelope = create_test_batch_envelope(1);
        map.add_job(envelope).await;

        let metadata = map.get_job_batch_metadata(1).await;
        assert!(metadata.is_some());
        assert_eq!(metadata.unwrap().batch_info.commit_info.batch_number, 1);

        let result = map
            .complete_job(1, crate::prover_api::metrics::ProverType::Real, "prover-1")
            .await;
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
            ProverStage::Snark,
        ));
        map.add_job(create_test_batch_envelope(1)).await;
        map.add_job(create_test_batch_envelope(3)).await;

        // Both additions block while the queue spans its configured range. Once batch 1 is
        // removed they wake together; whichever inserts first, the other must re-check the key
        // under the lock and merge only the earlier age instead of replacing the entry.
        let aged_map = map.clone();
        let aged_add = tokio::spawn(async move {
            aged_map
                .add_job_with_age(create_test_batch_envelope(2), Duration::from_secs(5))
                .await;
        });
        let fresh_map = map.clone();
        let fresh_add = tokio::spawn(async move {
            fresh_map.add_job(create_test_batch_envelope(2)).await;
        });
        tokio::time::sleep(Duration::from_millis(25)).await;

        map.complete_job(1, ProverType::Real, "test")
            .await
            .expect("the head job must be removable");
        tokio::time::timeout(Duration::from_secs(1), async {
            aged_add.await.unwrap();
            fresh_add.await.unwrap();
        })
        .await
        .expect("both duplicate waiters must finish");

        let status = map.status().await;
        let batch_2 = status
            .iter()
            .find(|job| job.fri_job.batch_number == 2)
            .expect("batch 2 must be queued exactly once");
        assert!(batch_2.added_seconds_ago >= 5);
        assert_eq!(
            status
                .iter()
                .filter(|job| job.fri_job.batch_number == 2)
                .count(),
            1
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
        let (fri_job, _data) = job.unwrap();
        assert_eq!(fri_job.batch_number, 1);

        // Job 1 is now assigned, should pick job 2
        let job = map.pick_job(Duration::ZERO, "prover-2", |_| true).await;
        assert!(job.is_some());
        let (fri_job, _data) = job.unwrap();
        assert_eq!(fri_job.batch_number, 2);

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
        let (fri_job, _data) = job.unwrap();
        assert_eq!(fri_job.batch_number, 1);
        assert_eq!(fri_job.vk_hash, ProvingVersion::V8.vk_hash());

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
        let (fri_job, _data) = job.unwrap();
        assert_eq!(fri_job.batch_number, 1);
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
        let SnarkJobPick::Assigned(jobs) = pick else {
            panic!("target-sized range must be assigned")
        };
        assert_eq!(
            jobs.iter()
                .map(|(job, _)| job.batch_number)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
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
        let SnarkJobPick::Assigned(first_jobs) = first_pick else {
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
        let SnarkJobPick::Assigned(second_jobs) = second_pick else {
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
        let SnarkJobPick::Assigned(jobs) = pick else {
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
        let SnarkJobPick::Assigned(jobs) = pick else {
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
                SnarkJobPick::Assigned(jobs) => jobs.len(),
                SnarkJobPick::Waiting(_) | SnarkJobPick::Empty => 0,
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
        assert!(matches!(first, SnarkJobPick::Assigned(_)));

        let premature_retry = map
            .pick_ready_snark_jobs(100, 2, Duration::from_secs(3600), "prover-2", |_| true)
            .await;
        assert!(matches!(premature_retry, SnarkJobPick::Empty));

        tokio::time::sleep(Duration::from_millis(20)).await;
        let reclaimed = map
            .pick_ready_snark_jobs(100, 2, Duration::from_secs(3600), "prover-2", |_| true)
            .await;
        let SnarkJobPick::Assigned(jobs) = reclaimed else {
            panic!("timed-out SNARK assignment must be reclaimed")
        };
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].0.batch_number, 1);
        assert_eq!(jobs[1].0.batch_number, 2);
    }

    #[tokio::test]
    async fn exact_snark_assignment_rejects_subrange_without_consuming_jobs() {
        let map = ProverJobMap::new(Duration::from_secs(60), 100, ProverStage::Snark);
        map.add_job(create_test_batch_envelope(1)).await;
        map.add_job(create_test_batch_envelope(2)).await;

        let pick = map
            .pick_ready_snark_jobs(100, 2, Duration::from_secs(3600), "prover-1", |_| true)
            .await;
        assert!(matches!(pick, SnarkJobPick::Assigned(_)));

        let rejected = map
            .complete_assigned_many_jobs(
                1,
                1,
                crate::prover_api::metrics::ProverType::Real,
                "prover-1",
            )
            .await;
        assert!(rejected.is_none());
        assert!(map.get_job_batch_metadata(1).await.is_some());
        assert!(map.get_job_batch_metadata(2).await.is_some());

        let completed = map
            .complete_assigned_many_jobs(
                1,
                2,
                crate::prover_api::metrics::ProverType::Real,
                "prover-1",
            )
            .await
            .expect("the exact assigned range must remain completable");
        assert_eq!(completed.len(), 2);
    }

    #[tokio::test]
    async fn test_complete_many_jobs() {
        let map = ProverJobMap::new(Duration::from_secs(60), 100, ProverStage::Snark);

        map.add_job(create_test_batch_envelope(1)).await;
        map.add_job(create_test_batch_envelope(2)).await;
        map.add_job(create_test_batch_envelope(3)).await;

        let result = map
            .complete_many_jobs(
                1,
                3,
                crate::prover_api::metrics::ProverType::Real,
                "prover-1",
            )
            .await;
        assert!(result.is_some());
        let envelopes = result.unwrap();
        assert_eq!(envelopes.len(), 3);

        // All jobs should be removed
        assert!(map.get_job_batch_metadata(1).await.is_none());
        assert!(map.get_job_batch_metadata(2).await.is_none());
        assert!(map.get_job_batch_metadata(3).await.is_none());
    }

    #[tokio::test]
    async fn test_complete_many_jobs_with_missing() {
        let map = ProverJobMap::new(Duration::from_secs(60), 100, ProverStage::Snark);

        map.add_job(create_test_batch_envelope(1)).await;
        map.add_job(create_test_batch_envelope(3)).await;

        // Try to complete 1-3, but 2 is missing
        let result = map
            .complete_many_jobs(
                1,
                3,
                crate::prover_api::metrics::ProverType::Real,
                "prover-1",
            )
            .await;
        assert!(result.is_none());

        // Original jobs should still be there
        assert!(map.get_job_batch_metadata(1).await.is_some());
        assert!(map.get_job_batch_metadata(3).await.is_some());
    }

    #[tokio::test]
    async fn test_complete_many_jobs_rejects_inverted_range() {
        let map = ProverJobMap::new(Duration::from_secs(60), 100, ProverStage::Snark);

        map.add_job(create_test_batch_envelope(2)).await;

        let result = map
            .complete_many_jobs(
                2,
                1,
                crate::prover_api::metrics::ProverType::Real,
                "prover-1",
            )
            .await;

        assert!(result.is_none());
        assert!(map.get_job_batch_metadata(2).await.is_some());
    }

    #[tokio::test]
    async fn test_batch_range_limit() {
        use std::sync::Arc;

        let map = Arc::new(ProverJobMap::new(
            Duration::from_secs(60),
            5, // Small range limit
            ProverStage::Fri,
        ));

        // Add jobs up to the limit
        for i in 1..=5 {
            map.add_job(create_test_batch_envelope(i)).await;
        }

        // Try to add another job - should block until we complete one
        let map_clone = Arc::clone(&map);
        let add_task = tokio::spawn(async move {
            map_clone.add_job(create_test_batch_envelope(6)).await;
        });

        // Give it time to hit the limit
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Complete a job to make space
        map.complete_job(1, crate::prover_api::metrics::ProverType::Real, "prover-1")
            .await;

        // Now the add should succeed
        tokio::time::timeout(Duration::from_millis(500), add_task)
            .await
            .expect("add_job should complete after space is available")
            .expect("task should not panic");

        assert!(map.get_job_batch_metadata(6).await.is_some());
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
        let map = ProverJobMap::new(Duration::from_secs(60), 100, ProverStage::Fri);

        map.add_job(create_test_batch_envelope(1)).await;

        let result1 = map
            .complete_job(1, crate::prover_api::metrics::ProverType::Real, "prover-1")
            .await;
        assert!(result1.is_some());

        let result2 = map
            .complete_job(1, crate::prover_api::metrics::ProverType::Real, "prover-1")
            .await;
        assert!(result2.is_none());
    }
}
