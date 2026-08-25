use crate::service::{PeerVerifyBatch, PeerVerifyBatchResult};
use alloy::primitives::Address;
use reth_network_peers::PeerId;
use tokio::sync::{broadcast, mpsc};

/// Dependencies required to run the main-node side of the `zks_2fa` subprotocol.
#[derive(Debug, Clone)]
pub struct MainNode2faConfig {
    /// SYSCOIN: Execution chain bound into the verifier-auth transcript.
    pub chain_id: u64,
    /// SYSCOIN: Local authenticated RLPx identity bound into the V2 verifier transcript.
    pub local_peer_id: PeerId,
    /// Accepted verifier signers for this main node. SYSCOIN: Signer authorization is exclusive
    /// across PeerIds but does not itself bypass the pre-authentication connection cap.
    pub accepted_verifier_signers: Vec<Address>,
    /// Channel used to forward batch verification results back into the node.
    pub verify_result_tx: mpsc::Sender<PeerVerifyBatchResult>,
}

/// Verifier identity and transport used by an external node participating in `zks_2fa`.
#[derive(Debug, Clone)]
pub struct ExternalNode2faConfig {
    /// SYSCOIN: Execution chain bound into the verifier-auth transcript.
    pub chain_id: u64,
    /// SYSCOIN: Local verifier RLPx identity bound into the V2 verifier transcript.
    pub local_peer_id: PeerId,
    /// SYSCOIN: RLPx-authenticated boot-node identities are the only peers allowed to request
    /// verifier work. An empty set intentionally rejects every peer.
    pub trusted_main_node_peers: Vec<PeerId>,
    pub signing_key: secrecy::SecretString,
    pub verify_batch_tx: mpsc::Sender<PeerVerifyBatch>,
    pub outgoing_verify_results: broadcast::Sender<PeerVerifyBatchResult>,
}
