mod map;
mod models;
mod tracked_lock;

// SYSCOIN: Export the atomic target-or-age SNARK readiness result to the job manager.
pub use map::{ProverJobMap, SnarkJobPick};
pub(super) use models::JobEntry;
