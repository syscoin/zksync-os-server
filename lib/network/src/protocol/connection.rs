// SYSCOIN: Exact replay lifecycle ownership carries shared handler state and RLPx session facts.
use super::{HandlerSharedState, ProtocolEvent};
use alloy::primitives::bytes::BytesMut;
use reth_network::Direction;
use reth_network_peers::PeerId;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::{OwnedSemaphorePermit, mpsc};

/// Outbound protocol frame plus optional replay-flow-control state.
///
/// SYSCOIN: Replay frames can be much larger than control frames, so the main-node replay producer
/// attaches a permit that remains held until the frame is drained from the outbound channel.
pub(crate) struct OutboundMessage {
    bytes: BytesMut,
    _replay_queue_permit: Option<OwnedSemaphorePermit>,
}

impl OutboundMessage {
    pub(crate) fn control(bytes: BytesMut) -> Self {
        Self {
            bytes,
            _replay_queue_permit: None,
        }
    }

    pub(crate) fn replay(bytes: BytesMut, replay_queue_permit: OwnedSemaphorePermit) -> Self {
        Self {
            bytes,
            _replay_queue_permit: Some(replay_queue_permit),
        }
    }

    fn into_bytes(self) -> BytesMut {
        self.bytes
    }
}

/// SYSCOIN: Owns one exact accepted replay token but publishes lifecycle only after the remote
/// endpoint proves it kept the same physical protocol stream. This filters crossed simultaneous
/// dials that Reth briefly marks active on opposite sockets at the two endpoints.
pub(crate) struct ReplayConnectionLifecycle {
    state: HandlerSharedState,
    direction: Direction,
    peer_id: PeerId,
    remote_addr: SocketAddr,
    token: u64,
    established: bool,
    twofa_activated: bool,
}

impl ReplayConnectionLifecycle {
    pub(crate) fn new(
        state: HandlerSharedState,
        direction: Direction,
        peer_id: PeerId,
        remote_addr: SocketAddr,
        token: u64,
    ) -> Self {
        Self {
            state,
            direction,
            peer_id,
            remote_addr,
            token,
            established: false,
            twofa_activated: false,
        }
    }

    pub(crate) fn events_sender(&self) -> mpsc::UnboundedSender<ProtocolEvent> {
        self.state.events_sender()
    }

    /// SYSCOIN: Publish `Established` once only after a role-specific message proves mutual use of
    /// this exact satellite stream. A closed local event consumer does not kill replay itself.
    pub(crate) fn establish(&mut self) {
        if self.established {
            return;
        }
        self.established = self
            .state
            .events_sender()
            .send(ProtocolEvent::Established {
                direction: self.direction,
                peer_id: self.peer_id,
                remote_addr: self.remote_addr,
            })
            .is_ok();
    }

    pub(crate) fn is_established(&self) -> bool {
        self.established
    }

    /// SYSCOIN: Start the exact optional verifier lane once. Callers choose the role-specific proof
    /// point while the shared registry preserves `Established`-before-verifier event ordering.
    pub(crate) fn activate_twofa(&mut self) {
        if self.twofa_activated {
            return;
        }
        self.twofa_activated = true;
        self.state
            .activate_twofa_session(self.peer_id, self.remote_addr);
    }
}

impl Drop for ReplayConnectionLifecycle {
    // SYSCOIN: Only the exact accepted replay owner may publish teardown lifecycle state.
    fn drop(&mut self) {
        self.state
            .finish_connection_if_owner(self.peer_id, self.token, self.established);
    }
}

/// The outbound side of a `zks` protocol connection.
///
/// Wraps an mpsc receiver fed by a background Tokio task (`run_mn_connection()` or
/// `run_en_connection()`) that owns the actual protocol logic. Dropping this struct aborts the
/// background task and releases the connection permit (if any; trusted peers hold none).
/// SYSCOIN: The task itself acquires exact accepted-session ownership before emitting lifecycle
/// state; a tentative duplicate or cap-rejected wrapper has no lifecycle authority.
pub struct ZksConnection {
    pub(crate) outbound_rx: mpsc::Receiver<OutboundMessage>,
    pub(crate) task: Option<tokio::task::JoinHandle<()>>,
    pub(crate) _permit: Option<OwnedSemaphorePermit>,
}

impl Drop for ZksConnection {
    // SYSCOIN: Session ownership is released by the task-local lifecycle guard; the wrapper only
    // aborts its own optional task and cannot unregister a replacement connection.
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

impl futures::Stream for ZksConnection {
    type Item = BytesMut;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.outbound_rx
            .poll_recv(cx)
            .map(|message| message.map(OutboundMessage::into_bytes))
    }
}
