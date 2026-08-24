pub mod fake_fri_provers_pool;
pub mod fri_job_manager;
mod fri_proof_verifier;
pub mod fri_proving_pipeline_step;
pub mod gapless_committer;
pub mod gapless_l1_proof_sender;
mod metrics;
pub mod proof_storage;
mod prover_job_map;
pub mod prover_server;
pub mod snark_job_manager;
// SYSCOIN: Verify real wrapper proofs against one canonical settlement-layer snapshot before any
// durable local acceptance or job consumption.
pub(crate) mod snark_proof_preflight;
// SYSCOIN: Keep accepted wrapper proofs crash-safe until their validated L1 receipt is confirmed.
pub(crate) mod snark_proof_journal;
pub mod snark_proving_pipeline_step;
#[cfg(test)]
mod test_util;
