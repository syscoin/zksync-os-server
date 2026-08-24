use super::config::{ExternalNode2faConfig, MainNode2faConfig};
// SYSCOIN: The wrapper retains typed-stream ownership while role workers can exit independently.
use super::en::drive_2fa_en_connection;
use super::mn::drive_2fa_mn_connection;
use super::wire::Zks2faMessage;
use crate::protocol::{HandlerSharedState, TWOFA_ACTIVATION_TIMEOUT};
use alloy::primitives::bytes::BytesMut;
use futures::{Future, Stream, StreamExt};
use reth_eth_wire::capability::SharedCapabilities;
use reth_eth_wire::multiplex::ProtocolConnection;
use reth_eth_wire::protocol::Protocol;
use reth_network::Direction;
use reth_network::protocol::{ConnectionHandler, OnNotSupported, ProtocolHandler};
use reth_network_peers::PeerId;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::task::{Context, Poll};
use tokio::sync::{OwnedSemaphorePermit, mpsc, oneshot, watch};
use tokio::time::Instant;
use tracing::Instrument;

/// Channel capacity for outbound `zks_2fa` protocol messages.
const OUTBOUND_CHANNEL_CAPACITY: usize = 32;

// SYSCOIN: Monotonic process-local generation IDs let the session tracker reject delayed events
// from a superseded connection even when the same authenticated PeerId reconnects.
static NEXT_ZKS_2FA_LANE_ID: AtomicU64 = AtomicU64::new(1);

/// Handle for sending messages to a peer over its live `zks_2fa` connection.
#[derive(Debug, Clone)]
pub struct Zks2faPeerHandle {
    // SYSCOIN: Unique generation of this exact live connection, carried on verifier events.
    lane_id: u64,
    /// Channel used to queue encoded protocol frames to the peer.
    outbound_tx: mpsc::Sender<BytesMut>,
    // SYSCOIN: The dispatcher and inbound connection task share this exact lane's authorization
    // and outstanding-request state; PeerSessionStore events are not an authorization boundary.
    lane_state: Arc<Mutex<Zks2faLaneState>>,
    // SYSCOIN: Closing or replacing a lane wakes its exact worker out of every pending wait.
    cancellation_tx: watch::Sender<bool>,
    // SYSCOIN: Reservation changes wake the MN worker so it can arm or replace the exact deadline.
    request_revision_tx: watch::Sender<u64>,
}

// SYSCOIN: Authentication and request ownership are scoped to one live zks_2fa connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Zks2faLaneAuthorization {
    AwaitingRole,
    AwaitingAuth,
    Authorized,
    Closed,
}

// SYSCOIN: A reservation is owned by one request generation and carries its exact expiry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OutstandingVerifyRequest {
    request_id: u64,
    batch_number: u64,
    generation: u64,
    deadline: Instant,
}

// SYSCOIN: Connection-local state is the authorization and request-admission boundary.
#[derive(Debug)]
struct Zks2faLaneState {
    authorization: Zks2faLaneAuthorization,
    outstanding: Option<OutstandingVerifyRequest>,
    next_request_generation: u64,
    // SYSCOIN: Peer protocol faults, exact request timeout, or loss of a consumed result to shared
    // backpressure restart the owning RLPx session; local policy/lifecycle exits preserve replay.
    disconnect_session: bool,
}

// SYSCOIN: Dispatch failures are explicit so saturated or stale lanes fail closed without awaits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyDispatchError {
    LaneNotAuthorized,
    LaneClosed,
    Outstanding { request_id: u64, batch_number: u64 },
    OutboundFull,
    OutboundClosed,
    RequestExpired,
}

// SYSCOIN: Result admission distinguishes unauthorized, stale, and mismatched lane traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VerifyResultAdmissionError {
    LaneNotAuthorized,
    LaneClosed,
    NoOutstandingRequest,
    MismatchedRequest {
        expected_request_id: u64,
        expected_batch_number: u64,
    },
}

impl Zks2faPeerHandle {
    pub(crate) fn new(outbound_tx: mpsc::Sender<BytesMut>) -> Self {
        let (cancellation_tx, _) = watch::channel(false);
        let (request_revision_tx, _) = watch::channel(0);
        // SYSCOIN: Exact-generation identity must never wrap and alias a process-lifetime lane.
        let lane_id = NEXT_ZKS_2FA_LANE_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |lane_id| {
                lane_id.checked_add(1)
            })
            .expect("zks_2fa lane generation exhausted");
        Self {
            lane_id,
            outbound_tx,
            lane_state: Arc::new(Mutex::new(Zks2faLaneState {
                authorization: Zks2faLaneAuthorization::AwaitingRole,
                outstanding: None,
                next_request_generation: 1,
                disconnect_session: false,
            })),
            cancellation_tx,
            request_revision_tx,
        }
    }

    pub(crate) const fn lane_id(&self) -> u64 {
        self.lane_id
    }

    pub(crate) fn cancellation_receiver(&self) -> watch::Receiver<bool> {
        self.cancellation_tx.subscribe()
    }

    pub(crate) fn request_revision_receiver(&self) -> watch::Receiver<u64> {
        self.request_revision_tx.subscribe()
    }

    pub(crate) fn is_open(&self) -> bool {
        self.lane_state
            .lock()
            .expect("zks_2fa lane state lock poisoned")
            .authorization
            != Zks2faLaneAuthorization::Closed
    }

    pub(crate) fn is_authorized(&self) -> bool {
        self.lane_state
            .lock()
            .expect("zks_2fa lane state lock poisoned")
            .authorization
            == Zks2faLaneAuthorization::Authorized
    }

    // SYSCOIN: The optional-protocol wrapper retains this exact lane marker through active drain so
    // a late peer decoder fault still closes the owning RLPx session.
    pub(crate) fn requires_session_disconnect(&self) -> bool {
        self.lane_state
            .lock()
            .expect("zks_2fa lane state lock poisoned")
            .disconnect_session
    }

    // SYSCOIN: Enforce the one-way connection-local authorization state machine:
    // AwaitingRole -> AwaitingAuth -> Authorized -> Closed.
    pub(crate) fn begin_authentication(&self) -> bool {
        let mut state = self
            .lane_state
            .lock()
            .expect("zks_2fa lane state lock poisoned");
        if state.authorization != Zks2faLaneAuthorization::AwaitingRole {
            return false;
        }
        state.authorization = Zks2faLaneAuthorization::AwaitingAuth;
        true
    }

    pub(crate) fn authorize(&self) -> bool {
        let mut state = self
            .lane_state
            .lock()
            .expect("zks_2fa lane state lock poisoned");
        if state.authorization != Zks2faLaneAuthorization::AwaitingAuth {
            return false;
        }
        state.authorization = Zks2faLaneAuthorization::Authorized;
        true
    }

    pub(crate) fn close(&self) {
        {
            let mut state = self
                .lane_state
                .lock()
                .expect("zks_2fa lane state lock poisoned");
            state.authorization = Zks2faLaneAuthorization::Closed;
            state.outstanding = None;
        }
        self.cancellation_tx.send_replace(true);
        self.bump_request_revision();
    }

    // SYSCOIN: Peer protocol faults and an exact result lost after reservation consumption are not
    // safely reusable on this lane. Mark its owning RLPx session for disconnect/redial; callers use
    // plain `close` for local shutdown, replacement, auth policy, and other replay-safe outcomes.
    pub(crate) fn close_for_session_recovery(&self) {
        {
            let mut state = self
                .lane_state
                .lock()
                .expect("zks_2fa lane state lock poisoned");
            state.authorization = Zks2faLaneAuthorization::Closed;
            state.outstanding = None;
            state.disconnect_session = true;
        }
        self.cancellation_tx.send_replace(true);
        self.bump_request_revision();
    }

    // SYSCOIN: Registry cleanup must compare the connection-local state identity, not merely a
    // cloned outbound sender that could otherwise be paired with unrelated authorization state.
    fn same_lane(&self, other: &Self) -> bool {
        self.lane_id == other.lane_id
            && Arc::ptr_eq(&self.lane_state, &other.lane_state)
            && self.outbound_tx.same_channel(&other.outbound_tx)
    }

    // SYSCOIN: Wake deadline waiters on every reservation revision and expose only the currently
    // authorized generation's exact deadline.
    fn bump_request_revision(&self) {
        self.request_revision_tx
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }

    pub(crate) fn outstanding_deadline(&self) -> Option<(u64, Instant)> {
        let state = self
            .lane_state
            .lock()
            .expect("zks_2fa lane state lock poisoned");
        (state.authorization == Zks2faLaneAuthorization::Authorized)
            .then_some(state.outstanding)
            .flatten()
            .map(|outstanding| (outstanding.generation, outstanding.deadline))
    }

    /// SYSCOIN: Safely dispatch a typed verification request through this exact authorized lane.
    /// Reservation and bounded admission are atomic; callers never receive the raw frame sender.
    pub fn try_send_verify_batch(
        &self,
        request: crate::wire::verification::VerifyBatch,
        deadline: Instant,
    ) -> Result<(), VerifyDispatchError> {
        let request_id = request.request_id;
        let batch_number = request.batch_number;
        self.try_enqueue_verify_batch(
            request_id,
            batch_number,
            Zks2faMessage::VerifyBatch(request).encoded(),
            deadline,
        )
    }

    /// SYSCOIN: Expire only the exact generation whose timer fired. A consumed request, retry, or
    /// replacement cannot be cleared by a stale deadline future.
    pub(crate) fn expire_outstanding(&self, generation: u64) -> bool {
        let expired = {
            let mut state = self
                .lane_state
                .lock()
                .expect("zks_2fa lane state lock poisoned");
            let is_expired = state.authorization == Zks2faLaneAuthorization::Authorized
                && state.outstanding.is_some_and(|outstanding| {
                    outstanding.generation == generation && outstanding.deadline <= Instant::now()
                });
            if is_expired {
                state.authorization = Zks2faLaneAuthorization::Closed;
                state.outstanding = None;
                state.disconnect_session = true;
            }
            is_expired
        };
        if expired {
            self.cancellation_tx.send_replace(true);
            self.bump_request_revision();
        }
        expired
    }

    /// SYSCOIN: Atomically reserve this exact request and enqueue it while the registry read lock
    /// proves this is still the current authorized lane. A failed enqueue rolls back only the
    /// reservation created by this call and never waits behind a slow peer.
    pub(crate) fn try_enqueue_verify_batch(
        &self,
        request_id: u64,
        batch_number: u64,
        encoded: BytesMut,
        deadline: Instant,
    ) -> Result<(), VerifyDispatchError> {
        let mut state = self
            .lane_state
            .lock()
            .expect("zks_2fa lane state lock poisoned");
        match state.authorization {
            Zks2faLaneAuthorization::Authorized => {}
            Zks2faLaneAuthorization::Closed => return Err(VerifyDispatchError::LaneClosed),
            Zks2faLaneAuthorization::AwaitingRole | Zks2faLaneAuthorization::AwaitingAuth => {
                return Err(VerifyDispatchError::LaneNotAuthorized);
            }
        }
        // SYSCOIN: The collector creates one absolute attempt deadline before queueing. Never
        // reserve or emit work after that same budget has expired in dispatcher backlog.
        if deadline <= Instant::now() {
            return Err(VerifyDispatchError::RequestExpired);
        }
        if let Some(outstanding) = state.outstanding {
            return Err(VerifyDispatchError::Outstanding {
                request_id: outstanding.request_id,
                batch_number: outstanding.batch_number,
            });
        }
        let generation = state.next_request_generation;
        state.next_request_generation = state
            .next_request_generation
            .checked_add(1)
            .expect("zks_2fa request generation exhausted");
        let requested = OutstandingVerifyRequest {
            request_id,
            batch_number,
            generation,
            deadline,
        };
        state.outstanding = Some(requested);
        let send_result = self.outbound_tx.try_send(encoded);
        match send_result {
            Ok(()) => {
                drop(state);
                self.bump_request_revision();
                Ok(())
            }
            Err(error) => {
                if state.outstanding == Some(requested) {
                    state.outstanding = None;
                }
                match error {
                    mpsc::error::TrySendError::Full(_) => Err(VerifyDispatchError::OutboundFull),
                    mpsc::error::TrySendError::Closed(_) => {
                        Err(VerifyDispatchError::OutboundClosed)
                    }
                }
            }
        }
    }

    /// SYSCOIN: Consume a result exactly once only when it matches this authorized lane's current
    /// reservation. This check and consumption are atomic with dispatcher reservations.
    pub(crate) fn consume_verify_result(
        &self,
        request_id: u64,
        batch_number: u64,
    ) -> Result<(), VerifyResultAdmissionError> {
        let mut state = self
            .lane_state
            .lock()
            .expect("zks_2fa lane state lock poisoned");
        match state.authorization {
            Zks2faLaneAuthorization::Authorized => {}
            Zks2faLaneAuthorization::Closed => {
                return Err(VerifyResultAdmissionError::LaneClosed);
            }
            Zks2faLaneAuthorization::AwaitingRole | Zks2faLaneAuthorization::AwaitingAuth => {
                return Err(VerifyResultAdmissionError::LaneNotAuthorized);
            }
        }
        let result = match state.outstanding {
            Some(expected)
                if expected.request_id == request_id && expected.batch_number == batch_number =>
            {
                state.outstanding = None;
                Ok(())
            }
            Some(expected) => Err(VerifyResultAdmissionError::MismatchedRequest {
                expected_request_id: expected.request_id,
                expected_batch_number: expected.batch_number,
            }),
            None => Err(VerifyResultAdmissionError::NoOutstandingRequest),
        };
        drop(state);
        if result.is_ok() {
            self.bump_request_revision();
        }
        result
    }
}

/// SYSCOIN: Cancellation is level-triggered through `watch`, so subscribing after replacement
/// still observes the closed state and no worker can miss the wakeup.
pub(crate) async fn wait_for_lane_cancellation(receiver: &mut watch::Receiver<bool>) {
    let _ = receiver.wait_for(|cancelled| *cancelled).await;
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
    // SYSCOIN: The accepted-session bridge keys activation to this exact physical RLPx socket.
    remote_addr: SocketAddr,
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
        // SYSCOIN: Exact remote socket joins this handler to Reth's accepted-session event.
        remote_addr: SocketAddr,
        permit: Option<OwnedSemaphorePermit>,
    ) -> Zks2faConnectionHandler {
        Zks2faConnectionHandler {
            role: self.role.clone(),
            state: self.state.clone(),
            connection_registry: self.connection_registry.clone(),
            remote_addr,
            permit,
        }
    }

    fn try_establish_outgoing_connection(
        &self,
        socket_addr: SocketAddr,
        peer_id: PeerId,
    ) -> Option<Zks2faConnectionHandler> {
        // SYSCOIN: Known trusted verifier identities bypass pre-authentication cap pressure.
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

    fn on_incoming(&self, socket_addr: SocketAddr) -> Option<Self::ConnectionHandler> {
        // SYSCOIN: Reth does not expose an incoming peer's identity until `into_connection`.
        // Defer admission so a trusted verifier dialing the main node can bypass a full cap.
        Some(self.establish_connection(socket_addr, None))
    }

    fn on_outgoing(
        &self,
        socket_addr: SocketAddr,
        peer_id: PeerId,
    ) -> Option<Self::ConnectionHandler> {
        // SYSCOIN: Outgoing RLPx exposes the authenticated identity before handler admission.
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
        // SYSCOIN: Deferred incoming admission consumes the optional permit after PeerId reveal.
        mut self,
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
                    // SYSCOIN: Drain the bounded typed stream so optional-protocol rejection does
                    // not wedge or tear down an otherwise healthy replay session.
                    return self.rejected_connection(conn);
                }
            }
        } else {
            self.permit.take()
        };

        // Note: session lifecycle (`Established`/`Closed`) is intentionally owned by the `zks`
        // replay connection, which every verifier peer also has. Emitting those events here too
        // would double-count peers in `PeerSessionStore`. We only emit verifier-specific events.
        let events_sender = self.state.events_sender();
        let (outbound_tx, outbound_rx) = mpsc::channel(OUTBOUND_CHANNEL_CAPACITY);
        // SYSCOIN: The collector-owned absolute deadline is carried with each dispatch; a lane has
        // no independently restarted timeout configuration.
        let peer_handle = Zks2faPeerHandle::new(outbound_tx.clone());
        let conn = into_message_stream(conn, peer_handle.clone());
        let connection_registry = self.connection_registry.clone();
        let (worker_done_tx, worker_done_rx) = oneshot::channel();
        // SYSCOIN: Satellite streams are polled during Reth's tentative ETH handshake. Only the
        // exact `(PeerId, remote_addr)` accepted-session event may register or start this lane.
        let activation = self
            .state
            .twofa_session_activation(peer_id, self.remote_addr);
        let task_peer_handle = peer_handle.clone();
        let task_connection_registry = connection_registry.clone();

        let task = match self.role {
            Twofa2Role::MainNode(config) => tokio::spawn(
                async move {
                    if !activation.wait_for(TWOFA_ACTIVATION_TIMEOUT).await {
                        tracing::warn!(
                            %peer_id,
                            timeout = ?TWOFA_ACTIVATION_TIMEOUT,
                            "2FA activation was not observed; retiring exact lane"
                        );
                        return;
                    }
                    let Some(registration) = register_accepted_connection(
                        task_connection_registry,
                        peer_id,
                        task_peer_handle.clone(),
                    ) else {
                        // SYSCOIN: An accepted physical session must never displace an existing
                        // exact owner; force this inconsistent session through RLPx recovery.
                        task_peer_handle.close_for_session_recovery();
                        return;
                    };
                    let mut conn = conn;
                    drive_2fa_mn_connection(
                        &mut conn,
                        outbound_tx,
                        events_sender,
                        peer_id,
                        config,
                        task_peer_handle.clone(),
                    )
                    .await;
                    drop(registration);
                    let _ = worker_done_tx.send(());
                    // SYSCOIN: Local shutdown, replacement, and auth-policy rejection preserve the
                    // replay session, but must keep consuming this exact inbound protocol stream.
                    if !task_peer_handle.requires_session_disconnect() {
                        drain_inbound_stream(&mut conn).await;
                    }
                }
                .instrument(tracing::info_span!("zks_2fa_mn_connection", %peer_id)),
            ),
            Twofa2Role::ExternalNode(config) => tokio::spawn(
                async move {
                    if !activation.wait_for(TWOFA_ACTIVATION_TIMEOUT).await {
                        tracing::warn!(
                            %peer_id,
                            timeout = ?TWOFA_ACTIVATION_TIMEOUT,
                            "2FA activation was not observed; retiring exact lane"
                        );
                        return;
                    }
                    let mut conn = conn;
                    // SYSCOIN: Unlike the MN, the EN has only sent replay traffic at activation.
                    // Defer shared registry ownership until a challenge proves the remote kept
                    // this exact 2FA stream, leaving crossed simultaneous dials side-effect free.
                    let registration_handle = task_peer_handle.clone();
                    drive_2fa_en_connection(
                        &mut conn,
                        outbound_tx,
                        peer_id,
                        config,
                        task_peer_handle.clone(),
                        move || {
                            register_accepted_connection(
                                task_connection_registry,
                                peer_id,
                                registration_handle,
                            )
                        },
                    )
                    .await;
                    let _ = worker_done_tx.send(());
                    // SYSCOIN: Preserve replay for local policy exits without leaving the inbound
                    // satellite protocol unpolled behind an outbound-only keepalive.
                    if !task_peer_handle.requires_session_disconnect() {
                        drain_inbound_stream(&mut conn).await;
                    }
                }
                .instrument(tracing::info_span!("zks_2fa_en_connection", %peer_id)),
            ),
        };

        Zks2faConnection {
            outbound_rx,
            task: Some(task),
            worker_done_rx: Some(worker_done_rx),
            // SYSCOIN: The wrapper retains the exact tentative lane identity for synchronous,
            // idempotent cleanup; registry insertion remains deferred to accepted activation.
            registered_peer_id: Some(peer_id),
            registered_peer_handle: Some(peer_handle.clone()),
            session_peer_handle: Some(peer_handle),
            connection_registry,
            _permit: permit,
            _rejected_stream_keepalive: None,
        }
    }
}

impl Zks2faConnectionHandler {
    fn rejected_connection<S>(self, conn: S) -> Zks2faConnection
    where
        S: Stream<Item = BytesMut> + Unpin + Send + 'static,
    {
        let (outbound_tx, outbound_rx) = mpsc::channel(1);
        let peer_handle = Zks2faPeerHandle::new(outbound_tx.clone());
        let decoder_handle = peer_handle.clone();
        // SYSCOIN: Admission-cap rejection is local policy, not peer misconduct. Preserve replay
        // while draining through the same typed capped decoder. Malformed frames still mark this
        // exact owning RLPx for recovery instead of receiving an unbounded raw drain exemption.
        let task = tokio::spawn(async move {
            let mut conn = into_message_stream(conn, decoder_handle);
            drain_inbound_stream(&mut conn).await;
        });
        Zks2faConnection {
            outbound_rx,
            task: Some(task),
            worker_done_rx: None,
            registered_peer_id: None,
            registered_peer_handle: None,
            session_peer_handle: Some(peer_handle),
            connection_registry: self.connection_registry,
            _permit: None,
            // A closed satellite stream closes the multiplexed RLPx connection. Keep this optional
            // protocol pending while the task above consumes and discards its inbound frames.
            _rejected_stream_keepalive: Some(outbound_tx),
        }
    }
}

/// The outbound side of a `zks_2fa` protocol connection.
///
/// Admitted connections wrap an mpsc receiver fed by a background task. A cap-rejected optional
/// subprotocol remains pending while a task drains inbound frames so it does not wedge or close the
/// shared RLPx connection.
/// SYSCOIN: When an admitted worker exits, the lane is unregistered and made inert while the
/// independent replay lane remains connected. Dropping this struct aborts any live task.
pub struct Zks2faConnection {
    outbound_rx: mpsc::Receiver<BytesMut>,
    task: Option<tokio::task::JoinHandle<()>>,
    // SYSCOIN: The worker signals before entering a replay-preserving inbound drain, allowing the
    // wrapper to release the registry lane and admission permit without aborting that drain.
    worker_done_rx: Option<oneshot::Receiver<()>>,
    registered_peer_id: Option<PeerId>,
    // SYSCOIN: Exact handle used for state closure and race-safe registry cleanup.
    registered_peer_handle: Option<Zks2faPeerHandle>,
    // SYSCOIN: Retain exact lane state after unregistering so a decoder fault during active drain
    // still wakes the wrapper through task completion and closes the owning RLPx session.
    session_peer_handle: Option<Zks2faPeerHandle>,
    connection_registry: Zks2faConnectionRegistry,
    _permit: Option<OwnedSemaphorePermit>,
    _rejected_stream_keepalive: Option<mpsc::Sender<BytesMut>>,
}

impl Zks2faConnection {
    // SYSCOIN: Close the exact registered handle before compare-and-remove so a stale wrapper
    // cannot mutate or unregister a replacement lane.
    fn unregister_current(&mut self) {
        let peer_id = self.registered_peer_id.take();
        let registered_peer_handle = self.registered_peer_handle.take();
        if let Some(registered_peer_handle) = registered_peer_handle.as_ref() {
            registered_peer_handle.close();
        }
        if let (Some(peer_id), Some(registered_peer_handle)) =
            (peer_id, registered_peer_handle.as_ref())
        {
            // SYSCOIN: A stale connection may be dropped after a replacement for the same PeerId
            // was registered. Compare channel identity so it cannot unregister the newer lane.
            unregister_connection_if_current(
                &self.connection_registry,
                peer_id,
                registered_peer_handle,
            );
        }
    }

    fn make_inert(&mut self) {
        // SYSCOIN: A terminal auth/protocol result ends this 2FA handler and releases its capped
        // slot, but an optional satellite stream returning `None` tears down the entire RLPx
        // session. Replace it with a pending inert stream so independent replay can continue.
        self.unregister_current();
        self._permit = None;
        if self._rejected_stream_keepalive.is_none() {
            let (keepalive, outbound_rx) = mpsc::channel(1);
            self.outbound_rx = outbound_rx;
            self._rejected_stream_keepalive = Some(keepalive);
        }
    }

    // SYSCOIN: A full-session recovery marker tears down the owning RLPx wrapper immediately;
    // replay-safe worker completion instead follows `make_inert` above.
    fn requires_session_disconnect(&self) -> bool {
        self.session_peer_handle
            .as_ref()
            .is_some_and(Zks2faPeerHandle::requires_session_disconnect)
    }

    fn close_owning_session(&mut self) {
        self.unregister_current();
        self._permit = None;
        self.worker_done_rx = None;
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl Drop for Zks2faConnection {
    // SYSCOIN: Drop closes and unregisters only this exact lane, then aborts only its own task.
    fn drop(&mut self) {
        self.unregister_current();
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

// SYSCOIN: Post-acceptance first-wins registration is defense in depth. A closed owner may be
// replaced, but an open owner's authorization and outstanding request remain intact.
fn try_register_connection_handle(
    connection_registry: &Zks2faConnectionRegistry,
    peer_id: PeerId,
    peer_handle: Zks2faPeerHandle,
) -> bool {
    let mut connection_registry = connection_registry
        .write()
        .expect("zks_2fa connection registry lock poisoned");
    if connection_registry
        .get(&peer_id)
        .is_some_and(Zks2faPeerHandle::is_open)
    {
        return false;
    }
    connection_registry.insert(peer_id, peer_handle);
    true
}

// SYSCOIN: Remove only the registry entry owned by this exact connection handle.
fn unregister_connection_if_current(
    connection_registry: &Zks2faConnectionRegistry,
    peer_id: PeerId,
    registered_peer_handle: &Zks2faPeerHandle,
) {
    let mut connection_registry = connection_registry
        .write()
        .expect("zks_2fa connection registry lock poisoned");
    let is_current = connection_registry
        .get(&peer_id)
        .is_some_and(|handle| handle.same_lane(registered_peer_handle));
    if is_current {
        connection_registry.remove(&peer_id);
    }
}

/// SYSCOIN: Task-owned exact registration closes and unregisters on every exit path, including
/// cancellation. Wrapper cleanup is independently idempotent for synchronous tentative teardown.
struct RegisteredZks2faLaneGuard {
    connection_registry: Zks2faConnectionRegistry,
    peer_id: PeerId,
    peer_handle: Zks2faPeerHandle,
}

impl Drop for RegisteredZks2faLaneGuard {
    fn drop(&mut self) {
        self.peer_handle.close();
        unregister_connection_if_current(
            &self.connection_registry,
            self.peer_id,
            &self.peer_handle,
        );
    }
}

// SYSCOIN: Registry ownership begins only after reth accepts the exact physical session. The
// first-wins insert is retained as defense in depth against inconsistent duplicate activation.
fn register_accepted_connection(
    connection_registry: Zks2faConnectionRegistry,
    peer_id: PeerId,
    peer_handle: Zks2faPeerHandle,
) -> Option<RegisteredZks2faLaneGuard> {
    try_register_connection_handle(&connection_registry, peer_id, peer_handle.clone()).then_some(
        RegisteredZks2faLaneGuard {
            connection_registry,
            peer_id,
            peer_handle,
        },
    )
}

impl Stream for Zks2faConnection {
    type Item = BytesMut;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // SYSCOIN: Fail closed before yielding another frame when this lane requires RLPx recovery.
        if self.requires_session_disconnect() {
            self.close_owning_session();
            return Poll::Ready(None);
        }

        // SYSCOIN: A replay-preserving worker exit is distinct from task completion because the
        // same task remains alive to drain inbound frames. Release lane state at this milestone.
        if let Some(worker_done_rx) = self.worker_done_rx.as_mut()
            && Pin::new(worker_done_rx).poll(cx).is_ready()
        {
            self.worker_done_rx = None;
            if self.requires_session_disconnect() {
                self.close_owning_session();
                return Poll::Ready(None);
            }
            self.make_inert();
        }

        // SYSCOIN: Poll the worker lifecycle so terminal authentication failures perform cleanup
        // instead of leaving a registry sender and connection-cap permit alive indefinitely.
        if let Some(task) = self.task.as_mut() {
            match Pin::new(task).poll(cx) {
                Poll::Ready(result) => {
                    if let Err(error) = result {
                        tracing::warn!(%error, "zks_2fa connection task failed");
                    }
                    self.task = None;
                    // SYSCOIN: Peer protocol faults, an unanswered exact request, or a consumed
                    // result lost to shared backpressure make this lane unrecoverable. Close only
                    // its owning RLPx connection; local lifecycle/policy exits preserve replay.
                    if self.requires_session_disconnect() {
                        self.close_owning_session();
                        return Poll::Ready(None);
                    }
                    self.make_inert();
                }
                Poll::Pending => {}
            }
        }
        // SYSCOIN: Replacement closes the old shared lane before its worker observes the
        // cancellation. Never drain already queued frames across that linearization boundary;
        // the polled JoinHandle above has registered the wakeup that completes cleanup.
        if self
            .registered_peer_handle
            .as_ref()
            .is_some_and(|handle| !handle.is_open())
        {
            return Poll::Pending;
        }
        self.outbound_rx.poll_recv(cx)
    }
}

/// Wraps a raw `ProtocolConnection` into a typed `Zks2faMessage` stream.
///
/// Decode errors are logged and terminate the stream (by returning `None`), matching the behaviour
/// of a closed connection.
fn into_message_stream<S>(
    conn: S,
    peer_handle: Zks2faPeerHandle,
) -> impl Stream<Item = Zks2faMessage> + Unpin + Send + 'static
where
    S: Stream<Item = BytesMut> + Unpin + Send + 'static,
{
    Box::pin(conn.scan(peer_handle, |peer_handle, raw| {
        let result = Zks2faMessage::decode_message(&mut &raw[..]);
        let frame_bytes = raw.len();
        let peer_handle = peer_handle.clone();
        async move {
            match result {
                Ok(msg) => {
                    // SYSCOIN: Never log raw verifier frames or decoded payloads; expose only the
                    // variant and bounded frame size needed for transport diagnostics.
                    tracing::trace!(message_id = ?msg.message_id(), frame_bytes, "processing peer zks_2fa message");
                    Some(msg)
                }
                Err(error) => {
                    tracing::info!(%error, frame_bytes, "error decoding peer zks_2fa message; terminating");
                    // SYSCOIN: Decode, canonicality, and raw-frame-cap failures are peer-originated
                    // transport violations and close this exact lane's full RLPx session.
                    peer_handle.close_for_session_recovery();
                    None
                }
            }
        }
    }))
}

// SYSCOIN: Replay-preserving local outcomes must keep polling their exact inbound protocol stream;
// otherwise an outbound-only keepalive leaves RLPx input queued forever and permits reusable DoS.
async fn drain_inbound_stream<S>(conn: &mut S)
where
    S: Stream + Unpin,
{
    while conn.next().await.is_some() {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::Address;
    use futures::{channel::mpsc as futures_mpsc, stream};
    use std::collections::HashSet;
    use std::time::Duration;

    const TEST_CHAIN_ID: u64 = 57_057;
    const TEST_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

    fn test_deadline() -> Instant {
        Instant::now() + TEST_REQUEST_TIMEOUT
    }

    // SYSCOIN: Decoder canonicality and frame-cap failures originate at the peer boundary and
    // therefore close the exact optional-protocol wrapper's full RLPx session.
    #[tokio::test]
    async fn decode_and_over_cap_faults_disconnect_exact_wrapper_session() {
        async fn assert_disconnect(frame: BytesMut) {
            let peer_id = PeerId::repeat_byte(0x71);
            let connection_registry = Arc::new(RwLock::new(HashMap::new()));
            let (outbound_tx, outbound_rx) = mpsc::channel(1);
            let handle = Zks2faPeerHandle::new(outbound_tx);
            assert!(try_register_connection_handle(
                &connection_registry,
                peer_id,
                handle.clone(),
            ));
            let decoder_handle = handle.clone();
            let task = tokio::spawn(async move {
                let mut conn = into_message_stream(stream::iter([frame]), decoder_handle);
                assert!(conn.next().await.is_none());
            });
            let mut connection = Zks2faConnection {
                outbound_rx,
                task: Some(task),
                worker_done_rx: None,
                registered_peer_id: Some(peer_id),
                registered_peer_handle: Some(handle.clone()),
                session_peer_handle: Some(handle.clone()),
                connection_registry: connection_registry.clone(),
                _permit: None,
                _rejected_stream_keepalive: None,
            };

            assert!(
                futures::future::poll_fn(|cx| Pin::new(&mut connection).poll_next(cx))
                    .await
                    .is_none()
            );
            assert!(handle.requires_session_disconnect());
            assert!(connection_registry.read().unwrap().is_empty());
        }

        let verify_batch_id = crate::twofa::wire::Zks2faMessageId::VerifyBatch.as_u8();
        assert_disconnect(BytesMut::from(&[verify_batch_id][..])).await;
        let mut oversized = BytesMut::zeroed(128 * 1024 + 1);
        oversized[0] = verify_batch_id;
        assert_disconnect(oversized).await;
    }

    // SYSCOIN: Local cap policy preserves replay only for valid typed traffic. A rejected peer
    // cannot obtain an unbounded raw-frame drain that ignores the zks_2fa decoder's per-variant cap.
    #[tokio::test]
    async fn cap_rejected_typed_drain_disconnects_on_over_cap_frame() {
        let (events_tx, _events_rx) = mpsc::unbounded_channel();
        let (verify_result_tx, _verify_result_rx) = mpsc::channel(1);
        let handler = Zks2faProtocolHandler::for_main_node(
            MainNode2faConfig {
                chain_id: TEST_CHAIN_ID,
                local_peer_id: PeerId::repeat_byte(0x70),
                accepted_verifier_signers: Vec::new(),
                verify_result_tx,
            },
            HandlerSharedState::new(events_tx, 0, HashSet::new()),
            Arc::new(RwLock::new(HashMap::new())),
        );
        let verify_batch_id = crate::twofa::wire::Zks2faMessageId::VerifyBatch.as_u8();
        let mut oversized = BytesMut::zeroed(128 * 1024 + 1);
        oversized[0] = verify_batch_id;
        let mut connection = handler
            .establish_connection("127.0.0.1:30303".parse().unwrap(), None)
            .rejected_connection(stream::iter([oversized]));
        let private_handle = connection
            .session_peer_handle
            .as_ref()
            .expect("typed policy drain retains a private fault marker")
            .clone();

        assert!(
            futures::future::poll_fn(|cx| Pin::new(&mut connection).poll_next(cx))
                .await
                .is_none()
        );
        assert!(private_handle.requires_session_disconnect());
        assert!(connection.connection_registry.read().unwrap().is_empty());
    }

    // SYSCOIN: A local graceful/policy exit releases registry and cap ownership while its task
    // remains alive and actively consumes inbound traffic; it must not close the replay session.
    #[tokio::test]
    async fn replay_preserving_worker_exit_keeps_active_inbound_drain() {
        let peer_id = PeerId::repeat_byte(0x72);
        let connection_registry = Arc::new(RwLock::new(HashMap::new()));
        let (outbound_tx, outbound_rx) = mpsc::channel(1);
        let handle = Zks2faPeerHandle::new(outbound_tx);
        assert!(try_register_connection_handle(
            &connection_registry,
            peer_id,
            handle.clone(),
        ));
        let (worker_done_tx, worker_done_rx) = oneshot::channel();
        let (inbound_tx, inbound_rx) = futures_mpsc::unbounded::<BytesMut>();
        let (drained_tx, mut drained_rx) = mpsc::channel(1);
        let task_handle = handle.clone();
        let task = tokio::spawn(async move {
            task_handle.close();
            let _ = worker_done_tx.send(());
            let mut inbound_rx = inbound_rx.inspect(move |frame| {
                drained_tx.try_send(frame.len()).unwrap();
            });
            drain_inbound_stream(&mut inbound_rx).await;
        });
        let semaphore = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = semaphore.clone().try_acquire_owned().unwrap();
        let mut connection = Zks2faConnection {
            outbound_rx,
            task: Some(task),
            worker_done_rx: Some(worker_done_rx),
            registered_peer_id: Some(peer_id),
            registered_peer_handle: Some(handle.clone()),
            session_peer_handle: Some(handle.clone()),
            connection_registry: connection_registry.clone(),
            _permit: Some(permit),
            _rejected_stream_keepalive: None,
        };

        futures::future::poll_fn(|cx| {
            assert!(Pin::new(&mut connection).poll_next(cx).is_pending());
            if connection.registered_peer_handle.is_none() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;
        assert_eq!(semaphore.available_permits(), 1);
        assert!(connection_registry.read().unwrap().is_empty());
        assert!(
            connection.task.is_some(),
            "the inbound drain must remain active"
        );
        assert!(!handle.requires_session_disconnect());

        inbound_tx
            .unbounded_send(BytesMut::from(&b"drain me"[..]))
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), drained_rx.recv())
                .await
                .unwrap(),
            Some(8)
        );
        assert!(
            futures::poll!(connection.next()).is_pending(),
            "active replay-preserving drain must leave the optional wrapper pending"
        );
        drop(inbound_tx);
    }

    #[test]
    fn trusted_outgoing_peer_bypasses_connection_cap() {
        let trusted_peer = PeerId::repeat_byte(1);
        let untrusted_peer = PeerId::repeat_byte(2);
        let (protocol_tx, _protocol_rx) = mpsc::unbounded_channel();
        let (verify_result_tx, _verify_result_rx) = mpsc::channel(1);
        let state = HandlerSharedState::new(protocol_tx, 1, HashSet::from([trusted_peer]));
        let handler = Zks2faProtocolHandler::for_main_node(
            MainNode2faConfig {
                chain_id: TEST_CHAIN_ID,
                local_peer_id: PeerId::repeat_byte(9),
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

    #[tokio::test]
    async fn unregistering_current_connection_releases_outbound_stream() {
        let peer_id = PeerId::repeat_byte(3);
        let connection_registry = Arc::new(RwLock::new(HashMap::new()));
        let (outbound_tx, mut outbound_rx) = mpsc::channel(1);
        let registered_peer_handle = Zks2faPeerHandle::new(outbound_tx.clone());
        connection_registry
            .write()
            .unwrap()
            .insert(peer_id, registered_peer_handle.clone());
        drop(outbound_tx);

        unregister_connection_if_current(&connection_registry, peer_id, &registered_peer_handle);
        drop(registered_peer_handle);

        assert!(connection_registry.read().unwrap().is_empty());
        assert!(
            outbound_rx.recv().await.is_none(),
            "unregistering must release the old outbound protocol stream"
        );
    }

    #[tokio::test]
    async fn completed_worker_makes_lane_inert_and_releases_slot() {
        let peer_id = PeerId::repeat_byte(5);
        let connection_registry = Arc::new(RwLock::new(HashMap::new()));
        let (outbound_tx, outbound_rx) = mpsc::channel(1);
        let registered_peer_handle = Zks2faPeerHandle::new(outbound_tx.clone());
        connection_registry
            .write()
            .unwrap()
            .insert(peer_id, registered_peer_handle.clone());
        drop(outbound_tx);
        let semaphore = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = semaphore.clone().try_acquire_owned().unwrap();
        let task = tokio::spawn(async {});
        tokio::task::yield_now().await;
        let mut connection = Zks2faConnection {
            outbound_rx,
            task: Some(task),
            worker_done_rx: None,
            registered_peer_id: Some(peer_id),
            registered_peer_handle: Some(registered_peer_handle.clone()),
            session_peer_handle: Some(registered_peer_handle),
            connection_registry: connection_registry.clone(),
            _permit: Some(permit),
            _rejected_stream_keepalive: None,
        };

        futures::future::poll_fn(|cx| {
            let next = Pin::new(&mut connection).poll_next(cx);
            assert!(
                next.is_pending(),
                "terminated optional 2FA lane must remain pending instead of closing RLPx"
            );
            if connection.task.is_none() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;

        assert!(connection_registry.read().unwrap().is_empty());
        assert_eq!(semaphore.available_permits(), 1);
        assert!(connection.task.is_none());
        assert!(connection._rejected_stream_keepalive.is_some());
    }

    // SYSCOIN: Reth can construct two handlers before selecting one RLPx session. The tentative
    // loser must not cancel, replace, or consume the accepted lane's exact request state.
    #[test]
    fn duplicate_registration_is_first_wins_and_preserves_owner_state() {
        let peer_id = PeerId::repeat_byte(0x31);
        let connection_registry = Arc::new(RwLock::new(HashMap::new()));
        let (owner_tx, mut owner_rx) = mpsc::channel(1);
        let owner = authorized_handle(owner_tx);
        assert!(try_register_connection_handle(
            &connection_registry,
            peer_id,
            owner.clone(),
        ));
        owner
            .try_enqueue_verify_batch(31, 9, BytesMut::from(&b"owner"[..]), test_deadline())
            .unwrap();
        assert_eq!(owner_rx.try_recv().unwrap(), &b"owner"[..]);

        let (duplicate_tx, _duplicate_rx) = mpsc::channel(1);
        let duplicate = Zks2faPeerHandle::new(duplicate_tx);
        assert!(!try_register_connection_handle(
            &connection_registry,
            peer_id,
            duplicate.clone(),
        ));
        unregister_connection_if_current(&connection_registry, peer_id, &duplicate);

        assert!(owner.is_open());
        assert!(owner.is_authorized());
        assert!(matches!(
            owner.try_enqueue_verify_batch(
                32,
                10,
                BytesMut::from(&b"duplicate"[..]),
                test_deadline(),
            ),
            Err(VerifyDispatchError::Outstanding {
                request_id: 31,
                batch_number: 9,
            })
        ));
        assert!(owner.consume_verify_result(31, 9).is_ok());
        assert!(
            connection_registry
                .read()
                .unwrap()
                .get(&peer_id)
                .is_some_and(|registered| registered.same_lane(&owner))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn expired_request_closes_owning_session_and_releases_slot() {
        let peer_id = PeerId::repeat_byte(0x41);
        let connection_registry = Arc::new(RwLock::new(HashMap::new()));
        let (outbound_tx, mut outbound_rx) = mpsc::channel(1);
        let handle = authorized_handle(outbound_tx);
        assert!(try_register_connection_handle(
            &connection_registry,
            peer_id,
            handle.clone(),
        ));
        handle
            .try_enqueue_verify_batch(41, 7, BytesMut::from(&b"request"[..]), test_deadline())
            .unwrap();
        outbound_rx.try_recv().unwrap();
        let (generation, _) = handle.outstanding_deadline().unwrap();
        tokio::time::advance(TEST_REQUEST_TIMEOUT).await;
        assert!(handle.expire_outstanding(generation));

        let semaphore = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = semaphore.clone().try_acquire_owned().unwrap();
        let task = tokio::spawn(async {});
        tokio::task::yield_now().await;
        let mut connection = Zks2faConnection {
            outbound_rx,
            task: Some(task),
            worker_done_rx: None,
            registered_peer_id: Some(peer_id),
            registered_peer_handle: Some(handle.clone()),
            session_peer_handle: Some(handle),
            connection_registry: connection_registry.clone(),
            _permit: Some(permit),
            _rejected_stream_keepalive: None,
        };

        assert!(
            futures::future::poll_fn(|cx| Pin::new(&mut connection).poll_next(cx))
                .await
                .is_none(),
            "request expiry must close its exact RLPx connection for renegotiation"
        );
        assert_eq!(semaphore.available_permits(), 1);
        assert!(connection_registry.read().unwrap().is_empty());
        assert!(connection._rejected_stream_keepalive.is_none());
    }

    // SYSCOIN: A result consumed before bounded-channel saturation has no safe retry token. Its
    // recovery marker must make the optional wrapper close the full RLPx session, not park an inert
    // lane that can never receive another request.
    #[tokio::test]
    async fn result_channel_recovery_marker_closes_owning_session_for_redial() {
        let peer_id = PeerId::repeat_byte(0x43);
        let connection_registry = Arc::new(RwLock::new(HashMap::new()));
        let (outbound_tx, outbound_rx) = mpsc::channel(1);
        let handle = authorized_handle(outbound_tx);
        assert!(try_register_connection_handle(
            &connection_registry,
            peer_id,
            handle.clone(),
        ));
        let task_handle = handle.clone();
        let task = tokio::spawn(async move {
            task_handle.close_for_session_recovery();
        });
        tokio::task::yield_now().await;

        let semaphore = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = semaphore.clone().try_acquire_owned().unwrap();
        let mut connection = Zks2faConnection {
            outbound_rx,
            task: Some(task),
            worker_done_rx: None,
            registered_peer_id: Some(peer_id),
            registered_peer_handle: Some(handle.clone()),
            session_peer_handle: Some(handle),
            connection_registry: connection_registry.clone(),
            _permit: Some(permit),
            _rejected_stream_keepalive: None,
        };

        assert!(
            futures::future::poll_fn(|cx| Pin::new(&mut connection).poll_next(cx))
                .await
                .is_none(),
            "result-channel recovery must close the owning RLPx session"
        );
        assert_eq!(semaphore.available_permits(), 1);
        assert!(connection_registry.read().unwrap().is_empty());
        assert!(connection._rejected_stream_keepalive.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn expired_connection_cleanup_preserves_replacement_lane() {
        let peer_id = PeerId::repeat_byte(0x42);
        let connection_registry = Arc::new(RwLock::new(HashMap::new()));
        let (old_tx, mut old_rx) = mpsc::channel(1);
        let old_handle = authorized_handle(old_tx);
        assert!(try_register_connection_handle(
            &connection_registry,
            peer_id,
            old_handle.clone(),
        ));
        old_handle
            .try_enqueue_verify_batch(41, 7, BytesMut::from(&b"old request"[..]), test_deadline())
            .unwrap();
        old_rx.try_recv().unwrap();
        let (old_generation, _) = old_handle.outstanding_deadline().unwrap();
        tokio::time::advance(TEST_REQUEST_TIMEOUT).await;
        assert!(old_handle.expire_outstanding(old_generation));

        let semaphore = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = semaphore.clone().try_acquire_owned().unwrap();
        let task = tokio::spawn(async {});
        tokio::task::yield_now().await;
        let mut old_connection = Zks2faConnection {
            outbound_rx: old_rx,
            task: Some(task),
            worker_done_rx: None,
            registered_peer_id: Some(peer_id),
            registered_peer_handle: Some(old_handle.clone()),
            session_peer_handle: Some(old_handle),
            connection_registry: connection_registry.clone(),
            _permit: Some(permit),
            _rejected_stream_keepalive: None,
        };

        let (replacement_tx, mut replacement_rx) = mpsc::channel(1);
        let replacement = Zks2faPeerHandle::new(replacement_tx);
        assert!(try_register_connection_handle(
            &connection_registry,
            peer_id,
            replacement.clone(),
        ));
        assert!(replacement.begin_authentication());
        assert!(replacement.authorize());

        assert!(
            futures::future::poll_fn(|cx| Pin::new(&mut old_connection).poll_next(cx))
                .await
                .is_none(),
            "the expired wrapper must close only its own RLPx connection"
        );
        assert_eq!(semaphore.available_permits(), 1);
        assert!(
            connection_registry
                .read()
                .unwrap()
                .get(&peer_id)
                .is_some_and(|registered| registered.same_lane(&replacement)),
            "stale timeout cleanup must preserve the replacement registry generation"
        );
        replacement
            .try_enqueue_verify_batch(
                42,
                8,
                BytesMut::from(&b"replacement request"[..]),
                test_deadline(),
            )
            .unwrap();
        assert_eq!(
            replacement_rx.try_recv().unwrap(),
            &b"replacement request"[..]
        );
    }

    #[test]
    fn stale_connection_cannot_unregister_replacement() {
        let peer_id = PeerId::repeat_byte(4);
        let connection_registry = Arc::new(RwLock::new(HashMap::new()));
        let (stale_outbound_tx, _stale_outbound_rx) = mpsc::channel(1);
        let stale_registered_peer_handle = Zks2faPeerHandle::new(stale_outbound_tx);
        let (replacement_outbound_tx, _replacement_outbound_rx) = mpsc::channel(1);
        let replacement_peer_handle = Zks2faPeerHandle::new(replacement_outbound_tx.clone());
        connection_registry
            .write()
            .unwrap()
            .insert(peer_id, replacement_peer_handle);

        unregister_connection_if_current(
            &connection_registry,
            peer_id,
            &stale_registered_peer_handle,
        );

        let connection_registry = connection_registry.read().unwrap();
        let current = connection_registry
            .get(&peer_id)
            .expect("replacement connection must remain registered");
        assert!(
            current.outbound_tx.same_channel(&replacement_outbound_tx),
            "stale cleanup must preserve the replacement channel"
        );
    }

    #[test]
    fn sender_alias_with_different_lane_state_cannot_unregister_current() {
        let peer_id = PeerId::repeat_byte(6);
        let connection_registry = Arc::new(RwLock::new(HashMap::new()));
        let (outbound_tx, _outbound_rx) = mpsc::channel(1);
        let stale_handle = Zks2faPeerHandle::new(outbound_tx.clone());
        let current_handle = Zks2faPeerHandle::new(outbound_tx);
        connection_registry
            .write()
            .unwrap()
            .insert(peer_id, current_handle.clone());

        unregister_connection_if_current(&connection_registry, peer_id, &stale_handle);

        let registry = connection_registry.read().unwrap();
        assert!(
            registry
                .get(&peer_id)
                .is_some_and(|registered| registered.same_lane(&current_handle)),
            "sender identity alone must not authorize stale lane cleanup"
        );
    }

    #[test]
    fn open_owner_rejects_tentative_duplicate_without_state_change() {
        let peer_id = PeerId::repeat_byte(7);
        let connection_registry = Arc::new(RwLock::new(HashMap::new()));
        let (old_outbound_tx, _old_outbound_rx) = mpsc::channel(1);
        let old_handle = authorized_handle(old_outbound_tx);
        assert!(try_register_connection_handle(
            &connection_registry,
            peer_id,
            old_handle.clone(),
        ));

        let (new_outbound_tx, _new_outbound_rx) = mpsc::channel(1);
        let new_handle = Zks2faPeerHandle::new(new_outbound_tx);
        assert!(!try_register_connection_handle(
            &connection_registry,
            peer_id,
            new_handle.clone(),
        ));

        old_handle
            .try_enqueue_verify_batch(41, 7, BytesMut::from(&b"owner"[..]), test_deadline())
            .unwrap();
        assert_eq!(old_handle.consume_verify_result(41, 7), Ok(()));
        let registry = connection_registry.read().unwrap();
        assert!(
            registry
                .get(&peer_id)
                .is_some_and(|registered| registered.same_lane(&old_handle))
        );
        assert!(new_handle.is_open());
    }

    #[tokio::test(start_paused = true)]
    async fn duplicate_cannot_prevent_owner_expiry_then_closed_owner_can_be_replaced() {
        let peer_id = PeerId::repeat_byte(8);
        let connection_registry = Arc::new(RwLock::new(HashMap::new()));
        let (old_outbound_tx, mut old_outbound_rx) = mpsc::channel(1);
        let old_handle = authorized_handle(old_outbound_tx);
        old_handle
            .try_enqueue_verify_batch(41, 7, BytesMut::from(&b"old request"[..]), test_deadline())
            .unwrap();
        old_outbound_rx.try_recv().unwrap();
        let (old_generation, _) = old_handle.outstanding_deadline().unwrap();
        assert!(try_register_connection_handle(
            &connection_registry,
            peer_id,
            old_handle.clone(),
        ));

        let (replacement_tx, mut replacement_rx) = mpsc::channel(1);
        let replacement = Zks2faPeerHandle::new(replacement_tx);
        assert!(!try_register_connection_handle(
            &connection_registry,
            peer_id,
            replacement.clone(),
        ));

        tokio::time::advance(TEST_REQUEST_TIMEOUT).await;
        assert!(old_handle.expire_outstanding(old_generation));
        assert!(try_register_connection_handle(
            &connection_registry,
            peer_id,
            replacement.clone(),
        ));
        assert!(replacement.begin_authentication());
        assert!(replacement.authorize());
        replacement
            .try_enqueue_verify_batch(
                42,
                8,
                BytesMut::from(&b"replacement request"[..]),
                test_deadline(),
            )
            .unwrap();
        assert_eq!(
            replacement_rx.try_recv().unwrap(),
            &b"replacement request"[..]
        );
        assert!(
            connection_registry
                .read()
                .unwrap()
                .get(&peer_id)
                .is_some_and(|registered| registered.same_lane(&replacement))
        );
    }

    fn authorized_handle(outbound_tx: mpsc::Sender<BytesMut>) -> Zks2faPeerHandle {
        let handle = Zks2faPeerHandle::new(outbound_tx);
        assert!(handle.begin_authentication());
        assert!(handle.authorize());
        handle
    }

    #[test]
    fn dispatch_requires_connection_local_authorization() {
        let (outbound_tx, mut outbound_rx) = mpsc::channel(1);
        let handle = Zks2faPeerHandle::new(outbound_tx);

        assert_eq!(
            handle.try_enqueue_verify_batch(
                41,
                7,
                BytesMut::from(&b"before role"[..]),
                test_deadline(),
            ),
            Err(VerifyDispatchError::LaneNotAuthorized)
        );
        assert!(handle.begin_authentication());
        assert_eq!(
            handle.try_enqueue_verify_batch(
                41,
                7,
                BytesMut::from(&b"before auth"[..]),
                test_deadline(),
            ),
            Err(VerifyDispatchError::LaneNotAuthorized)
        );
        assert!(handle.authorize());
        handle
            .try_enqueue_verify_batch(41, 7, BytesMut::from(&b"authorized"[..]), test_deadline())
            .unwrap();
        assert_eq!(outbound_rx.try_recv().unwrap(), &b"authorized"[..]);
    }

    // SYSCOIN: An expired collector budget is rejected before reservation and leaves the exact
    // authorized lane reusable by the next attempt.
    #[tokio::test(start_paused = true)]
    async fn expired_deadline_is_rejected_without_reservation() {
        let (outbound_tx, mut outbound_rx) = mpsc::channel(1);
        let handle = authorized_handle(outbound_tx);

        assert_eq!(
            handle
                .try_enqueue_verify_batch(41, 7, BytesMut::from(&b"expired"[..]), Instant::now(),),
            Err(VerifyDispatchError::RequestExpired),
        );
        assert!(outbound_rx.try_recv().is_err());
        handle
            .try_enqueue_verify_batch(42, 8, BytesMut::from(&b"live"[..]), test_deadline())
            .expect("expired dispatch must not retain the reservation");
        assert_eq!(outbound_rx.try_recv().unwrap(), &b"live"[..]);
    }

    #[test]
    fn full_outbound_queue_rolls_back_exact_reservation() {
        let (outbound_tx, mut outbound_rx) = mpsc::channel(1);
        outbound_tx
            .try_send(BytesMut::from(&b"occupied"[..]))
            .unwrap();
        let handle = authorized_handle(outbound_tx);

        let error = handle
            .try_enqueue_verify_batch(41, 7, BytesMut::from(&b"first"[..]), test_deadline())
            .unwrap_err();
        assert_eq!(error, VerifyDispatchError::OutboundFull);
        assert_eq!(outbound_rx.try_recv().unwrap(), &b"occupied"[..]);
        handle
            .try_enqueue_verify_batch(42, 8, BytesMut::from(&b"second"[..]), test_deadline())
            .expect("failed enqueue must roll back its exact reservation");
        assert_eq!(outbound_rx.try_recv().unwrap(), &b"second"[..]);
    }

    #[test]
    fn result_must_match_and_consumes_reservation_once() {
        let (outbound_tx, _outbound_rx) = mpsc::channel(1);
        let handle = authorized_handle(outbound_tx);
        handle
            .try_enqueue_verify_batch(41, 7, BytesMut::from(&b"request"[..]), test_deadline())
            .unwrap();

        assert_eq!(
            handle.consume_verify_result(99, 7),
            Err(VerifyResultAdmissionError::MismatchedRequest {
                expected_request_id: 41,
                expected_batch_number: 7,
            })
        );
        handle.consume_verify_result(41, 7).unwrap();
        assert_eq!(
            handle.consume_verify_result(41, 7),
            Err(VerifyResultAdmissionError::NoOutstandingRequest)
        );
    }
}
