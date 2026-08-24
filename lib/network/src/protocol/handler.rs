// SYSCOIN: Replay handlers defer lifecycle to an exact mutually proven RLPx connection.
use super::config::ExternalNodeProtocolConfig;
use super::connection::{ReplayConnectionLifecycle, ZksConnection};
use super::en::run_en_connection;
use super::handler_shared_state::HandlerSharedState;
use super::mn::run_mn_connection;
use crate::version::ZksProtocolVersionSpec;
use crate::wire::message::{ZKS_PROTOCOL, ZksMessage};
use futures::{Stream, StreamExt};
use reth_eth_wire::capability::SharedCapabilities;
use reth_eth_wire::multiplex::ProtocolConnection;
use reth_eth_wire::protocol::Protocol;
use reth_network::Direction;
use reth_network::protocol::{ConnectionHandler, OnNotSupported, ProtocolHandler};
use reth_network_peers::PeerId;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use tracing::Instrument;
use zksync_os_storage_api::ReadReplay;

/// Channel capacity for outbound protocol messages. Provides natural backpressure so the MN
/// does not produce records faster than the EN can consume them.
const OUTBOUND_CHANNEL_CAPACITY: usize = 32;
/// SYSCOIN: Replay responses can contain large transaction/preimage payloads. Limit queued replay
/// frames separately from the general outbound channel so control traffic keeps its existing buffer.
const REPLAY_OUTBOUND_CHANNEL_CAPACITY: usize = 1;

#[derive(Debug, Clone)]
enum ProtocolRole<Replay> {
    MainNode { replay: Replay },
    ExternalNode(ExternalNodeProtocolConfig),
}

/// Registers one version of the `zks` replay protocol for either the main-node or external-node
/// role.
///
/// Production registers `zks/5`. Tests may register more than one handler to exercise capability
/// negotiation and the test-only `zks/0` replay format.
#[derive(Debug, Clone)]
pub struct ZksProtocolHandler<P: ZksProtocolVersionSpec, Replay: Clone> {
    role: ProtocolRole<Replay>,
    /// Current state of the protocol.
    state: HandlerSharedState,
    _phantom: PhantomData<P>,
}

/// Turns a negotiated `zks` capability into the role-specific replay task for one peer.
pub struct ZksProtocolConnectionHandler<P: ZksProtocolVersionSpec, Replay: Clone> {
    role: ProtocolRole<Replay>,
    /// Current state of the protocol.
    state: HandlerSharedState,
    remote_addr: SocketAddr,
    /// Owned permit for a taken active connection slot, or `None` for a trusted peer that bypasses the cap.
    permit: Option<OwnedSemaphorePermit>,
    _phantom: PhantomData<P>,
}

impl<P: ZksProtocolVersionSpec, Replay: Clone> ZksProtocolHandler<P, Replay> {
    pub fn for_main_node(replay: Replay, state: HandlerSharedState) -> Self {
        Self {
            role: ProtocolRole::MainNode { replay },
            state,
            _phantom: Default::default(),
        }
    }

    pub fn for_external_node(
        _replay: Replay,
        config: ExternalNodeProtocolConfig,
        state: HandlerSharedState,
    ) -> Self {
        Self {
            role: ProtocolRole::ExternalNode(config),
            state,
            _phantom: Default::default(),
        }
    }

    fn establish_connection(
        &self,
        remote_addr: SocketAddr,
        permit: Option<OwnedSemaphorePermit>,
    ) -> ZksProtocolConnectionHandler<P, Replay> {
        ZksProtocolConnectionHandler {
            role: self.role.clone(),
            state: self.state.clone(),
            remote_addr,
            permit,
            _phantom: Default::default(),
        }
    }

    fn try_establish_connection(
        &self,
        socket_addr: SocketAddr,
        peer_id: PeerId,
    ) -> Option<ZksProtocolConnectionHandler<P, Replay>> {
        // SYSCOIN: Outgoing peer identity is already authenticated, so trusted replay peers bypass
        // cap pressure before capability negotiation just as they do at deferred incoming admission.
        if self.state.is_trusted(&peer_id) {
            return Some(self.establish_connection(socket_addr, None));
        }
        match self.state.try_acquire_connection_slot() {
            Ok(permit) => Some(self.establish_connection(socket_addr, Some(permit))),
            Err(_) => {
                tracing::warn!(
                    max_connections = self.state.max_active_connections(),
                    %socket_addr,
                    %peer_id,
                    "ignoring outgoing connection, max active reached"
                );
                self.state.emit_max_active_connections_exceeded();
                None
            }
        }
    }
}

impl<P: ZksProtocolVersionSpec, Replay: ReadReplay + Clone> ProtocolHandler
    for ZksProtocolHandler<P, Replay>
{
    type ConnectionHandler = ZksProtocolConnectionHandler<P, Replay>;

    fn on_incoming(&self, socket_addr: SocketAddr) -> Option<Self::ConnectionHandler> {
        // SYSCOIN: Reth exposes an incoming PeerId only in `into_connection`; defer admission so a
        // trusted replay peer remains connectable even while untrusted peers fill the cap.
        Some(self.establish_connection(socket_addr, None))
    }

    fn on_outgoing(
        &self,
        socket_addr: SocketAddr,
        peer_id: PeerId,
    ) -> Option<Self::ConnectionHandler> {
        // SYSCOIN: Outgoing identity is available early enough for trusted-cap admission.
        self.try_establish_connection(socket_addr, peer_id)
    }
}

impl<P: ZksProtocolVersionSpec, Replay: ReadReplay + Clone> ConnectionHandler
    for ZksProtocolConnectionHandler<P, Replay>
{
    type Connection = ZksConnection;

    fn protocol(&self) -> Protocol {
        ZksMessage::<P>::protocol()
    }

    fn on_unsupported_by_peer(
        self,
        supported: &SharedCapabilities,
        direction: Direction,
        peer_id: PeerId,
    ) -> OnNotSupported {
        // This handler is called because its exact `zks` version did not match. Another shared
        // `zks` capability means a different locally registered version can run the replay lane.
        // With no shared `zks` capability, the peer cannot replay and the whole session is rejected.
        if supported.iter_caps().any(|c| c.name() == ZKS_PROTOCOL) {
            OnNotSupported::KeepAlive
        } else {
            // Outdated peers keep redialing (backoff is deliberately short), so the log stays at
            // debug to avoid indefinite spam; the counter gives operators persistent visibility
            // of outdated peers without enabling debug logs.
            crate::metrics::ZKS_PROTOCOL_METRICS
                .unsupported_version_disconnects
                .inc();
            tracing::debug!(
                %peer_id,
                ?direction,
                "peer does not share any supported zks version; disconnecting"
            );
            OnNotSupported::Disconnect
        }
    }

    fn into_connection(
        self,
        direction: Direction,
        peer_id: PeerId,
        conn: ProtocolConnection,
    ) -> Self::Connection {
        // SYSCOIN: Incoming cap admission must occur after Reth reveals the authenticated PeerId.
        // A rejected mandatory replay capability ends only this tentative RLPx connection.
        let permit = if direction.is_incoming() && !self.state.is_trusted(&peer_id) {
            match self.state.try_acquire_connection_slot() {
                Ok(permit) => Some(permit),
                Err(_) => {
                    tracing::warn!(
                        max_connections = self.state.max_active_connections(),
                        %peer_id,
                        "rejecting incoming replay connection, max active reached"
                    );
                    self.state.emit_max_active_connections_exceeded();
                    return rejected_connection();
                }
            }
        } else {
            self.permit
        };

        let (outbound_tx, outbound_rx) = mpsc::channel(OUTBOUND_CHANNEL_CAPACITY);
        // SYSCOIN: Reth polls satellite streams during the pending ETH handshake, so stream polling
        // is not proof of acceptance. Wait for the exact `(PeerId, remote_addr)` active-session
        // event before claiming lifecycle ownership or running any replay protocol work.
        let activation = self.state.session_activation(peer_id, self.remote_addr);
        let state = self.state.clone();
        let remote_addr = self.remote_addr;

        let task = match self.role {
            ProtocolRole::MainNode { replay } => {
                // SYSCOIN: Reject response variants by their fixed ID before decoding an
                // untrusted EN-controlled replay-record payload.
                let conn = into_main_node_message_stream::<P>(conn);
                let replay_queue_permits =
                    Arc::new(Semaphore::new(REPLAY_OUTBOUND_CHANNEL_CAPACITY));
                tokio::spawn(
                    async move {
                        let Some(mut lifecycle) = activate_replay_connection(
                            activation,
                            state,
                            direction,
                            peer_id,
                            remote_addr,
                        )
                        .await
                        else {
                            return;
                        };
                        run_mn_connection::<P, _>(
                            conn,
                            outbound_tx,
                            replay_queue_permits,
                            lifecycle.events_sender(),
                            peer_id,
                            replay,
                            &mut lifecycle,
                        )
                        .await;
                    }
                    .instrument(tracing::info_span!("mn_connection", %peer_id)),
                )
            }
            ProtocolRole::ExternalNode(config) => {
                let conn = into_message_stream::<P>(conn);
                tokio::spawn(
                    async move {
                        let Some(mut lifecycle) = activate_replay_connection(
                            activation,
                            state,
                            direction,
                            peer_id,
                            remote_addr,
                        )
                        .await
                        else {
                            return;
                        };
                        run_en_connection::<P>(
                            conn,
                            outbound_tx,
                            lifecycle.events_sender(),
                            peer_id,
                            config,
                            Some(&mut lifecycle),
                        )
                        .await;
                    }
                    .instrument(tracing::info_span!("en_connection", %peer_id)),
                )
            }
        };

        ZksConnection {
            outbound_rx,
            task: Some(task),
            _permit: permit,
        }
    }
}

// SYSCOIN: A cap-rejected mandatory replay handler ends immediately without owning a task, permit,
// registry token, or lifecycle event; Reth then tears down only the tentative RLPx.
fn rejected_connection() -> ZksConnection {
    let (_outbound_tx, outbound_rx) = mpsc::channel(1);
    ZksConnection {
        outbound_rx,
        task: None,
        _permit: None,
    }
}

// SYSCOIN: Reth activation admits protocol I/O and the exact first-wins token, while role-level
// proof in the worker decides whether this locally active socket may publish replay lifecycle.
async fn activate_replay_connection(
    activation: super::handler_shared_state::SessionActivationWaiter,
    state: HandlerSharedState,
    direction: Direction,
    peer_id: PeerId,
    remote_addr: SocketAddr,
) -> Option<ReplayConnectionLifecycle> {
    if !activation.wait_for(super::SESSION_ACTIVATION_TIMEOUT).await {
        tracing::warn!(
            %peer_id,
            %remote_addr,
            timeout = ?super::SESSION_ACTIVATION_TIMEOUT,
            "accepted-session activation was not observed; closing exact RLPx session"
        );
        return None;
    }
    let token = state.try_claim_connection(peer_id)?;
    Some(ReplayConnectionLifecycle::new(
        state,
        direction,
        peer_id,
        remote_addr,
        token,
    ))
}

/// Wraps a raw `ProtocolConnection` into a typed message stream.
///
/// Each incoming byte frame is decoded as a `ZksMessage`. Decode errors are logged and terminate
/// the stream (by returning `None`), matching the behaviour of a closed connection.
fn into_message_stream<P: ZksProtocolVersionSpec>(
    conn: ProtocolConnection,
) -> impl Stream<Item = ZksMessage<P>> + Unpin + Send + 'static {
    Box::pin(conn.scan((), |_, raw| {
        let result = ZksMessage::<P>::decode_message(&mut &raw[..]);
        async move {
            match result {
                Ok(msg) => {
                    // SYSCOIN: Replay requests carry bounded but attacker-controlled override
                    // bytes. Log only fixed metadata so reconnects cannot amplify log volume.
                    tracing::trace!(message_id = ?msg.message_id(), "processing peer message");
                    Some(msg)
                }
                Err(error) => {
                    tracing::info!(%error, "error decoding peer message; terminating");
                    None
                }
            }
        }
    }))
}

/// SYSCOIN: The main node accepts only replay requests. Variant gating before payload decode keeps
/// an untrusted EN from using the unexpected response arm as a nested-allocation primitive.
fn into_main_node_message_stream<P: ZksProtocolVersionSpec>(
    conn: ProtocolConnection,
) -> impl Stream<Item = ZksMessage<P>> + Unpin + Send + 'static {
    Box::pin(conn.scan((), |_, raw| {
        let result = ZksMessage::<P>::decode_main_node_message(&mut &raw[..]);
        async move {
            match result {
                Ok(msg) => {
                    tracing::trace!(?msg, "processing peer message");
                    Some(msg)
                }
                Err(error) => {
                    tracing::info!(%error, "error decoding peer message; terminating");
                    None
                }
            }
        }
    }))
}
