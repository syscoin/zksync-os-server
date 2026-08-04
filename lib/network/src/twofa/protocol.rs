use super::config::{ExternalNode2faConfig, MainNode2faConfig};
use super::en::run_2fa_en_connection;
use super::mn::run_2fa_mn_connection;
use super::wire::Zks2faMessage;
use crate::protocol::HandlerSharedState;
use alloy::primitives::bytes::BytesMut;
use futures::{Stream, StreamExt};
use reth_eth_wire::capability::SharedCapabilities;
use reth_eth_wire::multiplex::ProtocolConnection;
use reth_eth_wire::protocol::Protocol;
use reth_network::Direction;
use reth_network::protocol::{ConnectionHandler, OnNotSupported, ProtocolHandler};
use reth_network_peers::PeerId;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::task::{Context, Poll};
use tokio::sync::{OwnedSemaphorePermit, mpsc};
use tracing::Instrument;

/// Channel capacity for outbound `zks_2fa` protocol messages.
const OUTBOUND_CHANNEL_CAPACITY: usize = 32;

/// Handle for sending messages to a peer over its live `zks_2fa` connection.
#[derive(Debug, Clone)]
pub struct Zks2faPeerHandle {
    /// Channel used to queue encoded protocol frames to the peer.
    pub outbound_tx: mpsc::Sender<BytesMut>,
}

/// Registry of currently connected `zks_2fa` peers and their live outbound send handles.
pub type Zks2faConnectionRegistry = Arc<RwLock<HashMap<PeerId, Zks2faPeerHandle>>>;

#[derive(Debug, Clone)]
enum Twofa2Role {
    MainNode(MainNode2faConfig),
    ExternalNode(ExternalNode2faConfig),
}

#[derive(Debug, Clone)]
pub struct Zks2faProtocolHandler {
    role: Twofa2Role,
    state: HandlerSharedState,
    connection_registry: Zks2faConnectionRegistry,
}

pub struct Zks2faConnectionHandler {
    role: Twofa2Role,
    state: HandlerSharedState,
    connection_registry: Zks2faConnectionRegistry,
    /// Owned permit for a taken active connection slot, or `None` for a trusted peer.
    permit: Option<OwnedSemaphorePermit>,
}

impl Zks2faProtocolHandler {
    pub fn for_main_node(
        config: MainNode2faConfig,
        state: HandlerSharedState,
        connection_registry: Zks2faConnectionRegistry,
    ) -> Self {
        Self {
            role: Twofa2Role::MainNode(config),
            state,
            connection_registry,
        }
    }

    pub fn for_external_node(
        config: ExternalNode2faConfig,
        state: HandlerSharedState,
        connection_registry: Zks2faConnectionRegistry,
    ) -> Self {
        Self {
            role: Twofa2Role::ExternalNode(config),
            state,
            connection_registry,
        }
    }

    fn establish_connection(
        &self,
        permit: Option<OwnedSemaphorePermit>,
    ) -> Zks2faConnectionHandler {
        Zks2faConnectionHandler {
            role: self.role.clone(),
            state: self.state.clone(),
            connection_registry: self.connection_registry.clone(),
            permit,
        }
    }

    fn try_establish_outgoing_connection(
        &self,
        socket_addr: SocketAddr,
        peer_id: PeerId,
    ) -> Option<Zks2faConnectionHandler> {
        if self.state.is_trusted(&peer_id) {
            return Some(self.establish_connection(None));
        }
        match self.state.try_acquire_connection_slot() {
            Ok(permit) => Some(self.establish_connection(Some(permit))),
            Err(_) => {
                tracing::warn!(
                    max_connections = self.state.max_active_connections(),
                    %socket_addr,
                    %peer_id,
                    "ignoring outgoing zks_2fa connection, max active reached"
                );
                self.state.emit_max_active_connections_exceeded();
                None
            }
        }
    }
}

impl ProtocolHandler for Zks2faProtocolHandler {
    type ConnectionHandler = Zks2faConnectionHandler;

    fn on_incoming(&self, _socket_addr: SocketAddr) -> Option<Self::ConnectionHandler> {
        // SYSCOIN: Reth does not expose an incoming peer's identity until `into_connection`.
        // Defer admission so a trusted verifier dialing the main node can bypass a full cap.
        Some(self.establish_connection(None))
    }

    fn on_outgoing(
        &self,
        socket_addr: SocketAddr,
        peer_id: PeerId,
    ) -> Option<Self::ConnectionHandler> {
        self.try_establish_outgoing_connection(socket_addr, peer_id)
    }
}

impl ConnectionHandler for Zks2faConnectionHandler {
    type Connection = Zks2faConnection;

    fn protocol(&self) -> Protocol {
        Zks2faMessage::protocol()
    }

    fn on_unsupported_by_peer(
        self,
        _supported: &SharedCapabilities,
        _direction: Direction,
        _peer_id: PeerId,
    ) -> OnNotSupported {
        // `zks_2fa` is an optional sub-protocol; replay-only peers must stay connected.
        OnNotSupported::KeepAlive
    }

    fn into_connection(
        self,
        direction: Direction,
        peer_id: PeerId,
        conn: ProtocolConnection,
    ) -> Self::Connection {
        let permit = if direction.is_incoming() && !self.state.is_trusted(&peer_id) {
            match self.state.try_acquire_connection_slot() {
                Ok(permit) => Some(permit),
                Err(_) => {
                    tracing::warn!(
                        max_connections = self.state.max_active_connections(),
                        %peer_id,
                        "ignoring incoming zks_2fa connection, max active reached"
                    );
                    self.state.emit_max_active_connections_exceeded();
                    drop(conn);
                    return self.rejected_connection();
                }
            }
        } else {
            self.permit
        };

        // Note: session lifecycle (`Established`/`Closed`) is intentionally owned by the `zks`
        // replay connection, which every verifier peer also has. Emitting those events here too
        // would double-count peers in `PeerSessionStore`. We only emit verifier-specific events.
        let events_sender = self.state.events_sender();
        let (outbound_tx, outbound_rx) = mpsc::channel(OUTBOUND_CHANNEL_CAPACITY);
        self.connection_registry
            .write()
            .expect("zks_2fa connection registry lock poisoned")
            .insert(
                peer_id,
                Zks2faPeerHandle {
                    outbound_tx: outbound_tx.clone(),
                },
            );
        let conn = into_message_stream(conn);
        let connection_registry = self.connection_registry.clone();

        let task = match self.role {
            Twofa2Role::MainNode(config) => tokio::spawn(
                run_2fa_mn_connection(conn, outbound_tx, events_sender, peer_id, config)
                    .instrument(tracing::info_span!("zks_2fa_mn_connection", %peer_id)),
            ),
            Twofa2Role::ExternalNode(config) => tokio::spawn(
                run_2fa_en_connection(conn, outbound_tx, peer_id, config)
                    .instrument(tracing::info_span!("zks_2fa_en_connection", %peer_id)),
            ),
        };

        Zks2faConnection {
            outbound_rx,
            task: Some(task),
            registered_peer_id: Some(peer_id),
            connection_registry,
            _permit: permit,
            _rejected_stream_keepalive: None,
        }
    }
}

impl Zks2faConnectionHandler {
    fn rejected_connection(self) -> Zks2faConnection {
        let (outbound_tx, outbound_rx) = mpsc::channel(1);
        Zks2faConnection {
            outbound_rx,
            task: None,
            registered_peer_id: None,
            connection_registry: self.connection_registry,
            _permit: None,
            // A closed satellite stream closes the multiplexed RLPx connection. Keep this optional
            // protocol pending instead, while its dropped inbound channel discards 2FA frames.
            _rejected_stream_keepalive: Some(outbound_tx),
        }
    }
}

/// The outbound side of a `zks_2fa` protocol connection.
///
/// Admitted connections wrap an mpsc receiver fed by a background task. A cap-rejected optional
/// subprotocol remains pending without a task so it does not close the shared RLPx connection.
/// Dropping this struct unregisters any admitted peer and aborts its task.
pub struct Zks2faConnection {
    outbound_rx: mpsc::Receiver<BytesMut>,
    task: Option<tokio::task::JoinHandle<()>>,
    registered_peer_id: Option<PeerId>,
    connection_registry: Zks2faConnectionRegistry,
    _permit: Option<OwnedSemaphorePermit>,
    _rejected_stream_keepalive: Option<mpsc::Sender<BytesMut>>,
}

impl Drop for Zks2faConnection {
    fn drop(&mut self) {
        if let Some(peer_id) = self.registered_peer_id {
            self.connection_registry
                .write()
                .expect("zks_2fa connection registry lock poisoned")
                .remove(&peer_id);
        }
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

impl Stream for Zks2faConnection {
    type Item = BytesMut;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.outbound_rx.poll_recv(cx)
    }
}

/// Wraps a raw `ProtocolConnection` into a typed `Zks2faMessage` stream.
///
/// Decode errors are logged and terminate the stream (by returning `None`), matching the behaviour
/// of a closed connection.
fn into_message_stream(
    conn: ProtocolConnection,
) -> impl Stream<Item = Zks2faMessage> + Unpin + Send + 'static {
    Box::pin(conn.scan((), |_, raw| {
        let result = Zks2faMessage::decode_message(&mut &raw[..]);
        async move {
            match result {
                Ok(msg) => {
                    tracing::trace!(?msg, "processing peer zks_2fa message");
                    Some(msg)
                }
                Err(error) => {
                    tracing::info!(%error, "error decoding peer zks_2fa message; terminating");
                    None
                }
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::Address;
    use std::collections::HashSet;

    #[test]
    fn trusted_outgoing_peer_bypasses_connection_cap() {
        let trusted_peer = PeerId::repeat_byte(1);
        let untrusted_peer = PeerId::repeat_byte(2);
        let (protocol_tx, _protocol_rx) = mpsc::unbounded_channel();
        let (verify_result_tx, _verify_result_rx) = mpsc::channel(1);
        let state = HandlerSharedState::new(protocol_tx, 1, HashSet::from([trusted_peer]));
        let handler = Zks2faProtocolHandler::for_main_node(
            MainNode2faConfig {
                accepted_verifier_signers: Vec::<Address>::new(),
                verify_result_tx,
            },
            state,
            Arc::new(RwLock::new(HashMap::new())),
        );
        let socket_addr = "127.0.0.1:30303".parse().unwrap();

        let _untrusted_connection = handler
            .try_establish_outgoing_connection(socket_addr, untrusted_peer)
            .expect("the first untrusted connection should fill the cap");

        assert!(
            handler
                .try_establish_outgoing_connection(socket_addr, trusted_peer)
                .is_some(),
            "a trusted outgoing verifier must remain connectable when the cap is full"
        );
        assert!(
            handler
                .try_establish_outgoing_connection(socket_addr, untrusted_peer)
                .is_none(),
            "an untrusted outgoing peer must remain subject to the cap"
        );
    }
}
