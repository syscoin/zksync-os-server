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
use zksync_os_l1_sender::batcher_model::{BatchMetadata, SignedBatchEnvelope};

/// Concurrent map of prover jobs that support FRI and SNARK workflows.
/// Imposes a limit on batch range
/// Keys are batch numbers stored in a BTreeMap for ordered iteration.
/// Values are prover input -
/// concrete type depend on the prover stage (FRI - prover_input (Vec<u32>), SNARK - fri_proof).
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
/// We don't maintain the SNARK job grouping - so that on timeout, a wider range can be assigned.
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
        let batch_number = batch_envelope.batch_number();
        let mut jobs = self.lock_with_tracking(JobMapMethod::AddJob).await;

        // Wait until there's space available (await if batch range limit would be exceeded)
        while self.is_queue_full(&jobs) {
            let queue_statistics = self.compute_and_record_statistics(&jobs);

            tracing::info!(
                batch_number,
                ?queue_statistics,
                ?self.prover_stage,
                max_assigned_batch_range = self.max_assigned_batch_range,
                "Waiting for space in job map"
            );
            // Drop lock before awaiting notification
            drop(jobs);
            self.space_available.notified().await;
            // Re-acquire lock after notification
            jobs = self.lock_with_tracking(JobMapMethod::AddJob).await;
        }

        let entry = JobEntry {
            metadata: JobMetadata::new_from_batch(&batch_envelope),
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

    /// Picks the first job (lowest batch number) that is either:
    /// - Pending and older than min_age (fake provers use non-empty min_age)
    /// - Assigned and timed out
    ///
    /// Returns None if no eligible job is found.
    ///
    /// Used for FRI jobs (one batch == one job)
    pub async fn pick_job(&self, min_age: Duration, prover_id: &str) -> Option<(FriJob, T)> {
        let now = Instant::now();
        let mut result = self
            .pick_jobs_while(1, prover_id, |entry| {
                // min_age is non-zero only for fake provers
                // for real provers this is no-op - that is, we always take the oldest eligible job
                now.duration_since(entry.metadata.added_at) >= min_age
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
    pub async fn pick_jobs_while<F>(
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

            // Assign job
            entry.metadata.assign(now, prover_id.to_string());
            selected_jobs.push(entry.metadata.clone());
        }

        if selected_jobs.is_empty() {
            return Vec::new();
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

    /// If a job is present for a given batch_number, returns the corresponding BatchMetadata
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
        let mut jobs = self
            .lock_with_tracking(JobMapMethod::CompleteManyJobs)
            .await;
        // First, verify all jobs exist -
        // it's possible a different job with an overlapping set of proofs was submitted.
        for batch_number in batch_number_from..=batch_number_to {
            if !jobs.contains_key(&batch_number) {
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

        // Record Prometheus metrics
        match &stats.job_with_max_attempts_info {
            // only writing metrics for normal case - the last assigned prover reported result
            Some(assignment_info) if assignment_info.last_assigned_to == prover_id => {
                PROVER_METRICS.prove_time[&(self.prover_stage, prover_type, prover_id.to_string())]
                    // time since last assignment is proving time
                    .observe(assignment_info.time_since_last_assignment);
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
