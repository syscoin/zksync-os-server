mod completed_ownership;
mod map;
mod models;
mod recovery_boundary;
mod tracked_lock;

// SYSCOIN: Export opaque lease ownership together with atomic prover assignment results.
pub use map::{
    BeginSubmissionError, JobMapCapacityExceeded, LeasedJob, ProverJobMap, SnarkJobAdmission,
    SnarkJobEligibility, SnarkJobPick, SnarkOwnershipCompletion, SnarkOwnershipSeedError,
    SubmissionLease,
};
// SYSCOIN: Keep delayed FRI rollback ownership internal to the prover pipeline.
pub(crate) use map::ReservedFriJob;
pub(super) use models::JobEntry;
pub(crate) use recovery_boundary::{
    MAX_STARTUP_RECOVERY_RANGES, PlannedSnarkRange, StartupRecoveryBoundaryError,
    StartupRecoveryPlan,
};
