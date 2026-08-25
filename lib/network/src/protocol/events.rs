use alloy::primitives::{Address, B256, BlockNumber};
use reth_network::Direction;
use reth_network_peers::PeerId;
use std::net::SocketAddr;

#[derive(Debug)]
pub enum ProtocolEvent {
    /// Connection established.
    Established {
        /// Connection direction.
        direction: Direction,
        /// Peer ID.
        peer_id: PeerId,
        /// Remote socket address observed when establishing the connection.
        remote_addr: SocketAddr,
    },
    /// Connection closed.
    Closed {
        /// Peer ID.
        peer_id: PeerId,
    },
    /// Peer requested replay stream starting from a specific block.
    ReplayRequested {
        /// Peer ID.
        peer_id: PeerId,
        /// First block peer expects to receive.
        starting_block: BlockNumber,
    },
    /// Peer requested verifier role for this session.
    VerifierRoleRequested {
        /// Peer ID.
        peer_id: PeerId,
        /// SYSCOIN: Exact 2FA connection generation that produced this event.
        lane_id: u64,
    },
    /// Main node sent verifier challenge to peer.
    VerifierChallengeSent {
        /// Peer ID.
        peer_id: PeerId,
        /// SYSCOIN: Exact 2FA connection generation that produced this event.
        lane_id: u64,
        /// Challenge nonce.
        nonce: B256,
    },
    /// Peer proved control of an accepted verifier signer.
    VerifierAuthorized {
        /// Peer ID.
        peer_id: PeerId,
        /// SYSCOIN: Exact 2FA connection generation that produced this event.
        lane_id: u64,
        /// Recovered verifier signer.
        signer: Address,
    },
    /// Peer failed verifier authorization.
    VerifierUnauthorized {
        /// Peer ID.
        peer_id: PeerId,
        /// SYSCOIN: Exact 2FA connection generation that produced this event.
        lane_id: u64,
        /// Recovered signer if signature parsing succeeded.
        signer: Option<Address>,
    },
    /// Replay record for a specific block was sent to peer.
    ReplayBlockSent {
        /// Peer ID.
        peer_id: PeerId,
        /// Block number contained in the replay record.
        block_number: BlockNumber,
    },
    /// Number of max active connections exceeded. New connection was rejected.
    MaxActiveConnectionsExceeded {
        /// The max number of active connections.
        max_connections: usize,
    },
    /// External node's replay stream from this peer is no longer usable (no messages within the
    /// inactivity timeout, or the message stream terminated while the session stayed up). The
    /// SYSCOIN: exact mandatory protocol wrapper closes itself; this event is observability only so
    /// a delayed PeerId-scoped consumer cannot disconnect a replacement session.
    ReplayStreamStalled {
        /// Peer ID.
        peer_id: PeerId,
        /// Next block the external node still expects to receive.
        next_block: BlockNumber,
    },
}
