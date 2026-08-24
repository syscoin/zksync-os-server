pub mod config;
pub(crate) mod metrics;
pub mod protocol;
pub mod raft;
pub mod service;
pub mod session;
pub mod twofa;
pub mod version;
mod wire;

// todo: temporary re-export while we have record overrides, otherwise `wire` module should be
//       entirely internal
// SYSCOIN: Expose the deadline-carrying verifier dispatch and exact lane-scoped message types to
// the node and batch-verification crates without duplicating their transport contract.
pub use service::{NetworkPorts, PeerVerifyBatch, PeerVerifyBatchResult, VerifyBatchDispatch};
pub use twofa::{
    ExternalNode2faConfig, MainNode2faConfig, ZKS_2FA_PROTOCOL, Zks2faMessage,
    Zks2faProtocolHandler,
};
pub use wire::replays::RecordOverride;
// SYSCOIN: Refusal producers and consumers share one byte-exact UTF-8-safe wire boundary.
pub use wire::verification::{
    MAX_VERIFY_BATCH_REFUSAL_REASON_BYTES, VerifyBatch, VerifyBatchOutcome, VerifyBatchResult,
    bounded_verify_batch_refusal_reason,
};

// Re-export relevant Reth types
pub use reth_network::config::SecretKey;
pub use reth_network::config::rng_secret_key;
pub use reth_network_peers::NodeRecord;
pub use reth_network_peers::PeerId;
pub use reth_network_peers::TrustedPeer;
