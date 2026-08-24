use alloy::primitives::B256;
use rand_core06::{OsRng, RngCore};
use std::fmt::Debug;
use std::time::{Duration, Instant};
use zksync_os_batch_types::batcher_model::SignedBatchEnvelope;
use zksync_os_types::ProvingVersion;

#[derive(Debug)]
pub struct JobEntry<T> {
    pub batch_envelope: SignedBatchEnvelope<T>,
    pub metadata: JobMetadata,
}

/// SYSCOIN: An OS-random capability authorizing one exact external prover assignment.
///
/// The custom `Debug` implementation is deliberately redacted so diagnostics cannot turn the
/// otherwise opaque capability into prover authority. Only pick responses expose its wire value.
#[derive(Clone, PartialEq, Eq)]
pub struct ProverLeaseToken(B256);

impl ProverLeaseToken {
    pub fn generate() -> Self {
        let mut bytes = [0_u8; 32];
        OsRng.fill_bytes(&mut bytes);
        Self(B256::from(bytes))
    }

    pub fn to_wire_value(&self) -> String {
        self.0.to_string()
    }

    pub fn matches_wire_value(&self, candidate: &str) -> bool {
        let Ok(candidate) = candidate.parse::<B256>() else {
            return false;
        };
        // SYSCOIN: Compare every byte so repeated network requests cannot learn a token prefix.
        self.0
            .as_slice()
            .iter()
            .zip(candidate.as_slice())
            .fold(0_u8, |difference, (expected, actual)| {
                difference | (expected ^ actual)
            })
            == 0
    }
}

impl Debug for ProverLeaseToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProverLeaseToken([REDACTED])")
    }
}

#[derive(Clone, Debug)]
pub struct JobMetadata {
    pub batch_number: u64,
    pub proving_version: ProvingVersion,
    // SYSCOIN: Release a real SNARK aggregation early once this range can advance interop.
    pub contains_interop_bundle: bool,
    // SYSCOIN: Exact marker-only durable JSON contribution, measured once before queue locking.
    // Aggregate admission sums this value atomically and can never lease an unpersistable range.
    pub durable_snark_batch_json_bytes: usize,
    pub tx_count: usize,
    pub computational_native_used: Option<u64>,
    pub added_at: Instant,
    pub assigned_to_prover_id: Option<String>,
    pub assigned_at: Option<Instant>,
    /// SYSCOIN: Exact aggregate lease; a prover may submit only this complete range.
    pub assigned_batch_range: Option<(u64, u64)>,
    /// SYSCOIN: Opaque authority for the current exact assignment; never expose through status.
    pub assigned_lease_token: Option<ProverLeaseToken>,
    /// SYSCOIN: Admit at most one submission for this lease before expensive verification.
    pub submission_in_progress: bool,
    pub current_attempt: usize, // 0 = never assigned, 1+ = assigned N times
}

pub enum QueueStatistics {
    Empty,
    NonEmpty(NonEmptyQueueStatistics),
}

pub struct NonEmptyQueueStatistics {
    pub min_batch_added_at: Instant,
    pub min_batch_current_attempt: usize,
    pub min_batch_number: u64,
    pub max_batch_number: u64,
    pub jobs_count: usize,
}

impl Debug for QueueStatistics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QueueStatistics::Empty => write!(f, "Empty queue"),
            QueueStatistics::NonEmpty(stats) => write!(
                f,
                "Queue has {} jobs, range: {} - {}, oldest job: added {:?} ago, has {} attempts.",
                stats.jobs_count,
                stats.min_batch_number,
                stats.max_batch_number,
                stats.min_batch_added_at.elapsed(),
                stats.min_batch_current_attempt
            ),
        }
    }
}

impl JobMetadata {
    pub fn new_from_batch<T>(batch_envelope: &SignedBatchEnvelope<T>) -> Self {
        Self::new_from_batch_with_age(batch_envelope, Duration::ZERO)
    }

    /// SYSCOIN: Reconstructs queue age for a job loaded from durable proof storage.
    pub fn new_from_batch_with_age<T>(
        batch_envelope: &SignedBatchEnvelope<T>,
        existing_age: Duration,
    ) -> Self {
        let batch_number = batch_envelope.batch_number();
        let proving_version = batch_envelope
            .batch
            .proving_version()
            .expect("Must be valid execution as set by the server");
        let contains_interop_bundle = batch_envelope.batch.contains_interop_bundle();
        let tx_count = batch_envelope.batch.tx_count;
        let computational_native_used = batch_envelope.batch.computational_native_used;
        let now = Instant::now();

        Self {
            batch_number,
            proving_version,
            contains_interop_bundle,
            // Populated before locking when this metadata enters the SNARK-stage map. FRI-stage
            // jobs never aggregate into a durable wrapper and avoid this extra serialization.
            durable_snark_batch_json_bytes: 0,
            tx_count,
            computational_native_used,
            // `existing_age` originates from a file timestamp and is expected to be small enough
            // to represent on the platform's monotonic clock. Falling back to `now` is defensive
            // for corrupt / unrepresentable timestamps; normal files preserve their prior age.
            added_at: now.checked_sub(existing_age).unwrap_or(now),
            assigned_to_prover_id: None,
            assigned_at: None,
            assigned_batch_range: None,
            assigned_lease_token: None,
            submission_in_progress: false,
            current_attempt: 0,
        }
    }

    /// SYSCOIN: Assign (or reassign) this job and its opaque capability atomically.
    pub fn assign(
        &mut self,
        assigned_at: Instant,
        assigned_to_prover_id: String,
        assigned_batch_range: (u64, u64),
        assigned_lease_token: ProverLeaseToken,
    ) {
        self.assigned_at = Some(assigned_at);
        self.assigned_to_prover_id = Some(assigned_to_prover_id);
        self.assigned_batch_range = Some(assigned_batch_range);
        self.assigned_lease_token = Some(assigned_lease_token);
        self.submission_in_progress = false;
        self.current_attempt += 1;
    }

    /// SYSCOIN: Clear the assignment, opaque capability, and submission guard together so the job
    /// can be picked up again immediately
    /// (e.g. after the assigned prover submitted a proof that failed verification).
    /// `current_attempt` is preserved as assignment history.
    pub fn unassign(&mut self) {
        self.assigned_at = None;
        self.assigned_to_prover_id = None;
        self.assigned_batch_range = None;
        self.assigned_lease_token = None;
        self.submission_in_progress = false;
    }
}

/// Statistics about a batch of jobs for logging and metrics
/// For FRI jobs - always one batch; for SNARK - can be multiple consecutive batches
pub struct JobBatchStats {
    pub min_batch_number: u64,
    pub max_batch_number: u64,
    pub proving_version: ProvingVersion,
    pub max_time_since_added: Duration,
    pub total_txs: usize,
    pub total_computational_native_used: Option<u64>,
    // present if at least one of the batches is currently assigned
    pub job_with_max_attempts_info: Option<PreviousAttemptsInfo>,
}

pub(super) struct PreviousAttemptsInfo {
    pub attempts: usize,
    pub time_since_last_assignment: Duration,
    pub last_assigned_to: String,
}

impl JobBatchStats {
    pub fn new(metadata_list: &[JobMetadata]) -> Self {
        assert!(!metadata_list.is_empty());

        let min_batch = &metadata_list[0];
        let max_batch_number = metadata_list[metadata_list.len() - 1].batch_number;
        // `unassign` keeps `current_attempt` as history but clears the assignment fields,
        // so `current_attempt > 0` does not imply the job is assigned.
        let job_with_max_attempts_info = metadata_list
            .iter()
            .filter(|m| m.current_attempt > 0)
            .filter_map(|m| {
                Some((
                    m.current_attempt,
                    m.assigned_at?,
                    m.assigned_to_prover_id.clone()?,
                ))
            })
            .max_by_key(|(attempts, ..)| *attempts)
            .map(
                |(attempts, assigned_at, last_assigned_to)| PreviousAttemptsInfo {
                    attempts,
                    time_since_last_assignment: assigned_at.elapsed(),
                    last_assigned_to,
                },
            );

        JobBatchStats {
            min_batch_number: min_batch.batch_number,
            max_batch_number,
            proving_version: min_batch.proving_version,
            max_time_since_added: min_batch.added_at.elapsed(),
            total_txs: metadata_list.iter().map(|m| m.tx_count).sum(),
            total_computational_native_used: metadata_list
                .iter()
                .map(|m| m.computational_native_used)
                .sum(),
            job_with_max_attempts_info,
        }
    }
    #[allow(dead_code)]
    fn format_batch_range(batch_numbers: &[u64]) -> String {
        match batch_numbers.len() {
            0 => String::from("none"),
            1 => format!("{}", batch_numbers[0]),
            _ => format!(
                "{}-{}",
                batch_numbers[0],
                batch_numbers[batch_numbers.len() - 1]
            ),
        }
    }
}

impl Debug for JobBatchStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.min_batch_number == self.max_batch_number {
            write!(f, "Batch {}", self.min_batch_number,)?;
        } else {
            write!(
                f,
                "{} Batches ({}-{})",
                self.max_batch_number - self.min_batch_number + 1,
                self.min_batch_number,
                self.max_batch_number,
            )?;
        }
        write!(
            f,
            " with {} txs, proving version {:?}, spent in queue: {:?}",
            self.total_txs, self.proving_version, self.max_time_since_added
        )?;
        if let Some(info) = &self.job_with_max_attempts_info {
            write!(
                f,
                ", last assigned to '{}', {} attempts, {:?} since last assignment",
                info.last_assigned_to, info.attempts, info.time_since_last_assignment
            )?;
        }
        Ok(())
    }
}
