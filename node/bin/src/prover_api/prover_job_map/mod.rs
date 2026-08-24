mod completed_ownership;
mod map;
mod models;
mod recovery_boundary;
mod tracked_lock;

// SYSCOIN: Export opaque lease ownership together with atomic prover assignment results.
pub use completed_ownership::SnarkCompletedOwner;
pub use map::{
    BeginSubmissionError, JobMapCapacityExceeded, LeasedJob, ProverJobMap, SnarkJobAdmission,
    SnarkJobEligibility, SnarkJobPick, SnarkOwnershipCompletion, SnarkOwnershipSeedError,
    SubmissionLease,
};
pub(super) use models::JobEntry;
pub(crate) use recovery_boundary::{
    PlannedSnarkRange, StartupRecoveryBoundaryError, StartupRecoveryPlan,
};
