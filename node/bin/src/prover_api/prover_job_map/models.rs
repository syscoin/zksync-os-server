use std::fmt::Debug;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct JobMetadata {
    pub batch_number: u64,
    pub vk_hash: String,
    pub tx_count: usize,
    pub added_at: Instant,
    pub assigned_at: Option<Instant>,
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
                "Queue has {} jobs, range: {} - {}, oldest job added {:?} ago and has {} attempts",
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
    pub fn new_pending(batch_number: u64, vk_hash: String, tx_count: usize) -> Self {
        Self {
            batch_number,
            vk_hash,
            tx_count,
            added_at: Instant::now(),
            assigned_at: None,
            current_attempt: 0,
        }
    }

    /// Assign (or reassign) this job to a prover.
    pub fn assign(&mut self, assigned_at: Instant) {
        self.assigned_at = Some(assigned_at);
        self.current_attempt += 1;
    }
}

/// Statistics about a batch of jobs for logging and metrics
/// For FRI jobs - always one batch; for SNARK - can be multiple consecutive batches
#[derive(Debug)]
#[allow(dead_code)] // used for debug
pub struct JobBatchStats {
    pub batch_number_range: String,
    pub vk_hash: String,
    pub max_attempts: usize,
    pub max_time_since_added: Duration,
    pub total_txs: usize,
    pub max_time_since_last_assignment: Option<Duration>,
}

impl JobBatchStats {
    pub fn new(metadata_list: &[JobMetadata]) -> Self {
        assert!(!metadata_list.is_empty());

        let first = &metadata_list[0];
        let batch_numbers: Vec<u64> = metadata_list.iter().map(|m| m.batch_number).collect();
        let max_attempts = metadata_list
            .iter()
            .map(|m| m.current_attempt)
            .max()
            .unwrap();

        let max_time_since_last_assignment: Option<Duration> = metadata_list
            .iter()
            .flat_map(|m| m.assigned_at.map(|a| a.elapsed()))
            .max();

        JobBatchStats {
            batch_number_range: Self::format_batch_range(&batch_numbers),
            vk_hash: first.vk_hash.clone(),
            max_attempts,
            max_time_since_added: first.added_at.elapsed(),
            total_txs: metadata_list.iter().map(|m| m.tx_count).sum(),
            max_time_since_last_assignment,
        }
    }
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
