use super::{ConnectionRegistry, ProtocolEvent};
use alloy::primitives::bytes::BytesMut;
use reth_network_peers::PeerId;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::{OwnedSemaphorePermit, mpsc};

/// Outbound protocol frame plus optional replay-flow-control state.
///
/// SYSCOIN: Replay frames can be much larger than control frames, so the main-node replay producer
/// attaches a permit that remains held until the frame is drained from the outbound channel.
pub struct OutboundMessage {
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

impl AsRef<[u8]> for OutboundMessage {
    fn as_ref(&self) -> &[u8] {
        self.bytes.as_ref()
    }
}

impl From<BytesMut> for OutboundMessage {
    fn from(bytes: BytesMut) -> Self {
        Self::control(bytes)
    }
}

/// The outbound side of a zks protocol connection.
///
/// Wraps an mpsc receiver fed by a background Tokio task (`run_mn_connection()` or
/// `run_en_connection()`) that owns the actual protocol logic. Dropping this struct aborts the
/// background task, unregisters the peer, emits `ProtocolEvent::Closed`, and releases the
/// connection permit.
pub struct ZksConnection {
    pub(crate) outbound_rx: mpsc::Receiver<OutboundMessage>,
    pub(crate) task: tokio::task::JoinHandle<()>,
    pub(crate) events_sender: mpsc::UnboundedSender<ProtocolEvent>,
    pub(crate) peer_id: PeerId,
    pub(crate) connection_registry: ConnectionRegistry,
    pub(crate) _permit: OwnedSemaphorePermit,
}

impl Drop for ZksConnection {
    fn drop(&mut self) {
        self.connection_registry
            .write()
            .expect("protocol connection registry lock poisoned")
            .remove(&self.peer_id);
        self.events_sender
            .send(ProtocolEvent::Closed {
                peer_id: self.peer_id,
            })
            .ok();
        self.task.abort();
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
