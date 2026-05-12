use crate::raft::wire::{RaftRequest, RaftResponse, RaftWireMessage, RequestId};
use async_trait::async_trait;
use dashmap::DashMap;
use futures::StreamExt;
use reth_eth_wire::multiplex::ProtocolConnection;
use reth_eth_wire::protocol::Protocol;
use reth_network::Direction;
use reth_network::protocol::{ConnectionHandler, OnNotSupported, ProtocolHandler};
use reth_network::types::Capability;
use reth_network_peers::PeerId;
use std::collections::HashSet;
use std::fmt::Debug;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Duration, Instant, sleep};
use tracing::Instrument;

pub const RAFT_PROTOCOL: &str = "zks_raft";
const RAFT_PROTOCOL_VERSION: usize = 1;
// RLPx multiplexing uses the first byte as a sub-protocol message id.
// Raft has two wire message kinds (request/response), so it needs 2 slots.
const RAFT_PROTOCOL_MESSAGE_COUNT: u8 = 2;
const RAFT_OUTBOUND_CHANNEL_CAPACITY: usize = 64;

#[derive(Debug)]
struct PendingRequest {
    // SYSCOIN: Keep the intended peer with the request so spoofed responses can be diagnosed.
    peer_id: PeerId,
    connection_id: u64,
    response_tx: oneshot::Sender<Result<RaftResponse, String>>,
}

#[async_trait]
pub trait RaftRequestHandler: Send + Sync + 'static {
    async fn handle(&self, request: RaftRequest) -> Result<RaftResponse, String>;
}

#[derive(Debug, Clone)]
pub struct RaftRouter {
    next_request_id: Arc<AtomicU64>,
    next_connection_id: Arc<AtomicU64>,
    // Stores (peer_id, connection_id, response_sender) so that when a connection drops we can
    // cancel all requests that were routed through it.
    pending: Arc<DashMap<RequestId, PendingRequest>>,
    // Vec<PeerChannel> rather than a single PeerChannel because devp2p can establish two
    // simultaneous TCP connections for the same peer when both nodes dial each other at the same
    // time (each node sees an incoming connection from the other while its own outgoing connection
    // is also completing). devp2p closes the unwanted duplicate shortly after, but there is a
    // brief window where both connections are alive. Storing all of them means whichever one
    // devp2p decides to keep stays in the router after the other is removed by its Drop calling
    // unregister_peer. With a single slot, one connection would be silently orphaned and the peer
    // would appear disconnected even though the kept connection is alive.
    peers: Arc<DashMap<PeerId, Vec<PeerChannel>>>,
    // SYSCOIN: Only configured consensus members may use the Raft subprotocol. RLPx authenticates
    // the PeerId; this allowlist authorizes that identity for consensus traffic.
    authorized_peers: Arc<HashSet<PeerId>>,
}

#[derive(Debug, Clone)]
struct PeerChannel {
    connection_id: u64,
    sender: mpsc::UnboundedSender<RaftWireMessage>,
}

impl Default for RaftRouter {
    fn default() -> Self {
        Self::new([])
    }
}

impl RaftRouter {
    pub fn new(authorized_peers: impl IntoIterator<Item = PeerId>) -> Self {
        Self {
            next_request_id: Arc::new(AtomicU64::new(1)),
            next_connection_id: Arc::new(AtomicU64::new(1)),
            pending: Arc::new(DashMap::new()),
            peers: Arc::new(DashMap::new()),
            authorized_peers: Arc::new(authorized_peers.into_iter().collect()),
        }
    }

    pub fn register_peer(
        &self,
        peer_id: PeerId,
        sender: mpsc::UnboundedSender<RaftWireMessage>,
    ) -> Result<u64, RaftTransportError> {
        if !self.is_authorized_peer(&peer_id) {
            tracing::warn!(%peer_id, "rejecting unauthorized raft peer connection");
            return Err(RaftTransportError::UnauthorizedPeer(peer_id));
        }

        let connection_id = self.next_connection_id.fetch_add(1, Ordering::Relaxed);
        self.peers.entry(peer_id).or_default().push(PeerChannel {
            connection_id,
            sender,
        });
        tracing::info!(%peer_id, connection_id, "raft peer connection registered");
        Ok(connection_id)
    }

    pub fn is_authorized_peer(&self, peer_id: &PeerId) -> bool {
        self.authorized_peers.contains(peer_id)
    }

    pub fn unregister_peer(&self, peer_id: &PeerId, connection_id: u64) {
        let mut entry = match self.peers.entry(*peer_id) {
            dashmap::mapref::entry::Entry::Occupied(e) => e,
            dashmap::mapref::entry::Entry::Vacant(_) => return,
        };
        let channels = entry.get_mut();
        let before = channels.len();
        channels.retain(|ch| ch.connection_id != connection_id);
        if channels.len() == before {
            // connection_id was not in the list; already removed or never stored
            return;
        }
        if channels.is_empty() {
            entry.remove();
            tracing::info!(%peer_id, connection_id, "raft peer unregistered (no remaining connections)");
        } else {
            tracing::debug!(%peer_id, connection_id, remaining = channels.len(), "raft connection unregistered, peer still has other connections");
        }
    }

    pub fn send_request(
        &self,
        peer_id: PeerId,
        req: RaftRequest,
    ) -> Result<oneshot::Receiver<Result<RaftResponse, String>>, RaftTransportError> {
        let Some(channels) = self.peers.get(&peer_id) else {
            tracing::debug!(%peer_id, connected = self.peers.len(), "raft request failed: peer not connected");
            return Err(RaftTransportError::NotConnected(peer_id));
        };
        let senders = channels.value().clone();
        drop(channels);

        let id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();

        let mut msg = RaftWireMessage::Request { id, req };
        for ch in &senders {
            match ch.sender.send(msg) {
                Ok(()) => {
                    self.pending.insert(
                        id,
                        PendingRequest {
                            peer_id,
                            connection_id: ch.connection_id,
                            response_tx: tx,
                        },
                    );
                    return Ok(rx);
                }
                Err(tokio::sync::mpsc::error::SendError(returned)) => {
                    // This channel is dead (receiver dropped); its Drop will call unregister_peer.
                    // Recover the message and try the next connection.
                    tracing::debug!(%peer_id, connection_id = ch.connection_id, "raft send failed on connection, trying next");
                    msg = returned;
                }
            }
        }

        tracing::debug!(%peer_id, request_id = id, "raft request failed: all connections dead");
        Err(RaftTransportError::SendFailed(peer_id))
    }

    pub fn connected_peers(&self) -> Vec<PeerId> {
        self.peers.iter().map(|entry| *entry.key()).collect()
    }

    pub async fn wait_for_peers(
        &self,
        peers: &[PeerId],
        timeout: Duration,
    ) -> Result<(), Vec<PeerId>> {
        let deadline = Instant::now() + timeout;
        let mut last_progress_log = Instant::now();
        loop {
            let connected = self.connected_peers();
            let missing: Vec<_> = peers
                .iter()
                .copied()
                .filter(|peer| !connected.contains(peer))
                .collect();

            if missing.is_empty() {
                tracing::info!(connected = ?connected, "all required raft peers are connected");
                return Ok(());
            }
            if Instant::now() >= deadline {
                tracing::warn!(missing = ?missing, connected = ?connected, "timed out waiting for raft peers");
                return Err(missing);
            }
            if last_progress_log.elapsed() >= Duration::from_secs(2) {
                tracing::info!(missing = ?missing, connected = ?connected, "still waiting for raft peers");
                last_progress_log = Instant::now();
            }

            sleep(Duration::from_millis(100)).await;
        }
    }

    // SYSCOIN: A response is valid only on the same authenticated connection that carried the
    // request. Request IDs alone are not an authentication boundary.
    pub fn complete_response(
        &self,
        id: RequestId,
        connection_id: u64,
        resp: Result<RaftResponse, String>,
    ) {
        let Some(entry) = self.pending.get(&id) else {
            return;
        };
        if entry.connection_id != connection_id {
            tracing::warn!(
                request_id = id,
                response_connection_id = connection_id,
                expected_connection_id = entry.connection_id,
                expected_peer_id = %entry.peer_id,
                "ignoring raft response from non-request connection"
            );
            return;
        }
        drop(entry);

        if let Some((_, entry)) = self.pending.remove(&id) {
            let _ = entry.response_tx.send(resp);
        }
    }

    fn cancel_pending_for_connection(&self, connection_id: u64) {
        let matching: Vec<RequestId> = self
            .pending
            .iter()
            .filter(|e| e.value().connection_id == connection_id)
            .map(|e| *e.key())
            .collect();
        for id in matching {
            if let Some((_, entry)) = self.pending.remove(&id) {
                let _ = entry
                    .response_tx
                    .send(Err(format!("connection {connection_id} dropped")));
            }
        }
    }
}

// SYSCOIN: Bind inbound Raft sender identity to the authenticated RLPx PeerId.
fn validate_request_peer(peer_id: PeerId, request: &RaftRequest) -> Result<(), String> {
    let claimed_peer_id = match request {
        RaftRequest::AppendEntries(req) => req.vote.leader_id.voted_for(),
        RaftRequest::Vote(req) => req.vote.leader_id.voted_for(),
        RaftRequest::InstallSnapshot(req) => req.vote.leader_id.voted_for(),
    };

    if claimed_peer_id == Some(peer_id) {
        return Ok(());
    }

    Err(format!(
        "raft request claimed sender {claimed_peer_id:?}, authenticated peer is {peer_id}"
    ))
}

#[derive(Debug, thiserror::Error)]
pub enum RaftTransportError {
    #[error("peer {0} is not connected")]
    NotConnected(PeerId),
    #[error("failed to send request to peer {0}")]
    SendFailed(PeerId),
    #[error("peer {0} is not authorized for raft")]
    UnauthorizedPeer(PeerId),
}

#[derive(Clone)]
pub struct RaftProtocolHandler {
    handler: Arc<dyn RaftRequestHandler>,
    router: RaftRouter,
}

impl Debug for RaftProtocolHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RaftProtocolHandler")
            .finish_non_exhaustive()
    }
}

impl RaftProtocolHandler {
    pub fn new(handler: impl RaftRequestHandler, router: RaftRouter) -> Self {
        Self {
            handler: Arc::new(handler),
            router,
        }
    }

    fn establish_connection(&self, peer_id: PeerId, conn: ProtocolConnection) -> RaftConnection {
        let (outbound_tx, outbound_rx) = mpsc::channel(RAFT_OUTBOUND_CHANNEL_CAPACITY);
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        let Ok(connection_id) = self.router.register_peer(peer_id, msg_tx) else {
            return RaftConnection::closed(peer_id, self.router.clone());
        };
        let task = tokio::spawn(
            run_raft_connection(
                peer_id,
                connection_id,
                conn,
                msg_rx,
                outbound_tx,
                self.handler.clone(),
                self.router.clone(),
            )
            .instrument(tracing::info_span!("raft_connection", %peer_id, connection_id)),
        );
        RaftConnection {
            peer_id,
            connection_id,
            router: self.router.clone(),
            outbound_rx,
            task,
        }
    }

    pub fn router(&self) -> RaftRouter {
        self.router.clone()
    }
}

impl ProtocolHandler for RaftProtocolHandler {
    type ConnectionHandler = RaftConnectionHandler;

    fn on_incoming(&self, _socket_addr: SocketAddr) -> Option<Self::ConnectionHandler> {
        tracing::debug!("incoming raft sub-protocol connection handler requested");
        Some(RaftConnectionHandler {
            handler: self.clone(),
        })
    }

    fn on_outgoing(
        &self,
        _socket_addr: SocketAddr,
        peer_id: PeerId,
    ) -> Option<Self::ConnectionHandler> {
        if !self.router.is_authorized_peer(&peer_id) {
            tracing::debug!(%peer_id, "skipping outgoing raft sub-protocol for unauthorized peer");
            return None;
        }

        tracing::debug!("outgoing raft sub-protocol connection handler requested");
        Some(RaftConnectionHandler {
            handler: self.clone(),
        })
    }
}

pub struct RaftConnectionHandler {
    handler: RaftProtocolHandler,
}

impl ConnectionHandler for RaftConnectionHandler {
    type Connection = RaftConnection;

    fn protocol(&self) -> Protocol {
        Protocol::new(
            Capability::new_static(RAFT_PROTOCOL, RAFT_PROTOCOL_VERSION),
            RAFT_PROTOCOL_MESSAGE_COUNT,
        )
    }

    fn on_unsupported_by_peer(
        self,
        _supported: &reth_eth_wire::capability::SharedCapabilities,
        _direction: Direction,
        _peer_id: PeerId,
    ) -> OnNotSupported {
        // Raft is an optional sub-protocol; non-raft peers should still be allowed to connect.
        OnNotSupported::KeepAlive
    }

    fn into_connection(
        self,
        direction: Direction,
        peer_id: PeerId,
        conn: ProtocolConnection,
    ) -> Self::Connection {
        tracing::info!(
            "raft sub-protocol connection established (direction={direction:?}, peer_id={peer_id})"
        );
        self.handler.establish_connection(peer_id, conn)
    }
}

/// Outbound side of a raft sub-protocol connection.
///
/// Wraps an mpsc receiver fed by a background Tokio task (`run_raft_connection`) that owns the
/// connection logic. Dropping this struct aborts the background task and cancels any pending
/// requests that were routed through this connection.
pub struct RaftConnection {
    peer_id: PeerId,
    connection_id: u64,
    router: RaftRouter,
    outbound_rx: mpsc::Receiver<alloy::primitives::bytes::BytesMut>,
    task: tokio::task::JoinHandle<()>,
}

impl futures::Stream for RaftConnection {
    type Item = alloy::primitives::bytes::BytesMut;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.outbound_rx.poll_recv(cx)
    }
}

impl Drop for RaftConnection {
    fn drop(&mut self) {
        tracing::info!(
            "raft connection dropped (peer_id={}, connection_id={}, pending_requests={})",
            self.peer_id,
            self.connection_id,
            self.router.pending.len(),
        );
        self.task.abort();
        self.router
            .cancel_pending_for_connection(self.connection_id);
        self.router
            .unregister_peer(&self.peer_id, self.connection_id);
    }
}

impl RaftConnection {
    fn closed(peer_id: PeerId, router: RaftRouter) -> Self {
        let (_outbound_tx, outbound_rx) = mpsc::channel(RAFT_OUTBOUND_CHANNEL_CAPACITY);
        let task = tokio::spawn(async {});
        Self {
            peer_id,
            connection_id: 0,
            router,
            outbound_rx,
            task,
        }
    }
}

async fn run_raft_connection(
    peer_id: PeerId,
    connection_id: u64,
    mut conn: ProtocolConnection,
    mut msg_rx: mpsc::UnboundedReceiver<RaftWireMessage>,
    outbound_tx: mpsc::Sender<alloy::primitives::bytes::BytesMut>,
    handler: Arc<dyn RaftRequestHandler>,
    router: RaftRouter,
) {
    loop {
        tokio::select! {
            frame = conn.next() => {
                let Some(bytes) = frame else {
                    tracing::info!(%peer_id, connection_id, "raft connection closed by peer");
                    break;
                };
                match RaftWireMessage::decode(&bytes[..]) {
                    Ok(RaftWireMessage::Request { id, req }) => {
                        tracing::debug!(%peer_id, request_id = id, "received raft request");
                        let handler = handler.clone();
                        let outbound_tx = outbound_tx.clone();
                        tokio::spawn(async move {
                            // SYSCOIN: Raft NodeId is the authenticated RLPx PeerId. Reject
                            // requests whose claimed Raft sender does not match this connection.
                            let resp = match validate_request_peer(peer_id, &req) {
                                Ok(()) => handler.handle(req).await,
                                Err(error) => Err(error),
                            };
                            let encoded = RaftWireMessage::Response { id, resp };
                            let buf = alloy::primitives::bytes::BytesMut::from(
                                encoded.encode().as_slice(),
                            );
                            let _ = outbound_tx.send(buf).await;
                        });
                    }
                    Ok(RaftWireMessage::Response { id, resp }) => {
                        tracing::debug!(%peer_id, request_id = id, "received raft response");
                        // SYSCOIN: Bind responses to the exact connection that carried the
                        // outbound request so another peer cannot satisfy guessed request IDs.
                        router.complete_response(id, connection_id, resp);
                    }
                    Err(error) => {
                        let preview_len = bytes.len().min(64);
                        let preview_hex = bytes[..preview_len]
                            .iter()
                            .map(|b| format!("{b:02x}"))
                            .collect::<String>();
                        tracing::warn!(
                            %peer_id,
                            connection_id,
                            %error,
                            msg_len = bytes.len(),
                            msg_preview_hex = %preview_hex,
                            "error decoding raft message; ignoring"
                        );
                    }
                }
            }
            msg = msg_rx.recv() => {
                let Some(msg) = msg else {
                    break;
                };
                let buf = alloy::primitives::bytes::BytesMut::from(msg.encode().as_slice());
                if outbound_tx.send(buf).await.is_err() {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        RaftRequest, RaftRouter, RaftTransportError, RaftWireMessage, validate_request_peer,
    };
    use alloy::primitives::b512;
    use openraft::raft::VoteRequest;
    use reth_network_peers::PeerId;
    use tokio::sync::oneshot::error::TryRecvError;

    fn peer_id(byte: u8) -> PeerId {
        PeerId::repeat_byte(byte)
    }

    fn vote_request(claimed_peer_id: PeerId) -> RaftRequest {
        RaftRequest::Vote(VoteRequest::new(
            openraft::Vote::new(1, claimed_peer_id),
            None,
        ))
    }

    #[test]
    fn raft_request_sender_must_match_authenticated_peer() {
        let authenticated_peer_id = b512!(
            "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001"
        );
        let other_peer_id = b512!(
            "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000002"
        );

        assert!(
            validate_request_peer(authenticated_peer_id, &vote_request(authenticated_peer_id))
                .is_ok()
        );
        assert!(
            validate_request_peer(authenticated_peer_id, &vote_request(other_peer_id)).is_err()
        );
    }

    #[test]
    fn raft_response_must_arrive_on_request_connection() {
        let peer1 = peer_id(0x11);
        let peer2 = peer_id(0x22);
        let router = RaftRouter::new([peer1, peer2]);
        let (peer1_tx, mut peer1_rx) = tokio::sync::mpsc::unbounded_channel();
        let (peer2_tx, _peer2_rx) = tokio::sync::mpsc::unbounded_channel();
        let peer1_connection_id = router
            .register_peer(peer1, peer1_tx)
            .expect("peer1 is authorized");
        let peer2_connection_id = router
            .register_peer(peer2, peer2_tx)
            .expect("peer2 is authorized");

        let mut response_rx = router
            .send_request(peer1, vote_request(peer1))
            .expect("peer1 is connected");
        let sent = peer1_rx.try_recv().expect("request sent to peer1");
        let RaftWireMessage::Request { id, .. } = sent else {
            panic!("expected raft request");
        };

        router.complete_response(id, peer2_connection_id, Err("spoofed".to_string()));
        assert!(matches!(response_rx.try_recv(), Err(TryRecvError::Empty)));

        router.complete_response(id, peer1_connection_id, Err("legitimate".to_string()));
        assert_eq!(
            response_rx
                .try_recv()
                .expect("response delivered")
                .expect_err("test sends an error response"),
            "legitimate"
        );
    }

    #[test]
    fn raft_router_rejects_unauthorized_peers() {
        let authorized_peer = peer_id(0x11);
        let unauthorized_peer = peer_id(0x22);
        let router = RaftRouter::new([authorized_peer]);
        let (authorized_tx, _authorized_rx) = tokio::sync::mpsc::unbounded_channel();
        let (unauthorized_tx, _unauthorized_rx) = tokio::sync::mpsc::unbounded_channel();

        router
            .register_peer(authorized_peer, authorized_tx)
            .expect("authorized peer is accepted");
        let error = router
            .register_peer(unauthorized_peer, unauthorized_tx)
            .expect_err("unauthorized peer is rejected");

        assert!(matches!(
            error,
            RaftTransportError::UnauthorizedPeer(peer) if peer == unauthorized_peer
        ));
        assert_eq!(router.connected_peers(), vec![authorized_peer]);
    }
}
