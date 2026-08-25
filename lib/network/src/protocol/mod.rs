//! Replay streaming over the `zks` RLPx subprotocol.
//!
//! Verifier authentication and batch verification live in [`crate::twofa`]. Both subprotocols
//! publish [`ProtocolEvent`]s here so the network service can join replay progress with verifier
//! state for the same peer.

mod config;
mod connection;
mod en;
mod events;
mod handler;
mod handler_shared_state;
mod mn;

pub use config::{
    ExternalNodeProtocolConfig, ExternalNodeVerifierConfig, MainNodeProtocolConfig,
    ZksProtocolConfig,
};
pub use connection::ZksConnection;
pub use events::ProtocolEvent;
pub use handler::{ZksProtocolConnectionHandler, ZksProtocolHandler};
// SYSCOIN: Test and service bridges share the exact accepted-session activation primitive.
pub use handler_shared_state::{HandlerSharedState, SessionActivationRegistry};

// SYSCOIN: Reth's accepted-session event stream is bounded and lossy on receiver lag. Protocol
// wrappers must therefore fail closed instead of retaining an accepted RLPx session forever if
// its exact activation edge is missed.
pub(crate) const SESSION_ACTIVATION_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(10);
// SYSCOIN: The MN may legitimately spend the full initial-request watchdog before releasing the
// companion verifier lane. Give that lane a strictly longer cleanup window so the two watchdogs
// cannot race and leave an otherwise healthy replay session without 2FA until reconnect.
pub(crate) const TWOFA_ACTIVATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Maximum number of replay records carried in a single `BlockReplays` message.
const MAX_BLOCKS_PER_MESSAGE: u64 = 64;
