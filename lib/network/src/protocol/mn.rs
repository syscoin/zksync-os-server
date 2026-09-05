use super::MAX_BLOCKS_PER_MESSAGE;
use super::ProtocolEvent;
// SYSCOIN: MN replay work publishes only through the exact mutually proven lifecycle owner.
use super::connection::{OutboundMessage, ReplayConnectionLifecycle};
use crate::version::ZksProtocolVersionSpec;
use crate::wire::message::ZksMessage;
use futures::{FutureExt, Stream, StreamExt};
use reth_network_peers::PeerId;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Semaphore, mpsc};
use zksync_os_storage_api::{ReadReplay, ReadReplayExt};

// SYSCOIN: Main-node replay responses may contain large records, so do not aggregate multiple
// records into one outbound frame before the network has applied backpressure.
const MAX_REPLAY_RECORDS_PER_RESPONSE: usize = 1;
// SYSCOIN: The EN sends its one replay request immediately after exact-session activation. Bound
// that first message so a silent authenticated peer cannot indefinitely occupy replay and deferred
// 2FA admission capacity while ordinary RLPx ping traffic keeps the connection alive.
const INITIAL_REPLAY_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Background task that drives the main-node side of a `zks` connection.
///
/// Waits for a `GetBlockReplays` request from the EN, then streams replay records from storage to
/// the EN indefinitely.
pub(super) async fn run_mn_connection<P: ZksProtocolVersionSpec, Replay: ReadReplay + Clone>(
    mut conn: impl Stream<Item = ZksMessage<P>> + Unpin,
    outbound_tx: mpsc::Sender<OutboundMessage>,
    replay_queue_permits: Arc<Semaphore>,
    events_sender: mpsc::UnboundedSender<ProtocolEvent>,
    peer_id: PeerId,
    replay: Replay,
    // SYSCOIN: Production MN work must own the exact mutually proven replay generation.
    lifecycle: &mut ReplayConnectionLifecycle,
) {
    // Receive the single GetBlockReplays request for this connection.
    let request = match tokio::time::timeout(INITIAL_REPLAY_REQUEST_TIMEOUT, conn.next()).await {
        Ok(Some(ZksMessage::GetBlockReplays(request))) => request,
        Ok(Some(msg)) => {
            tracing::info!(
                message_id = ?msg.message_id(),
                "received unexpected initial message from peer; terminating"
            );
            return;
        }
        Ok(None) => return,
        Err(_) => {
            tracing::warn!(
                %peer_id,
                timeout = ?INITIAL_REPLAY_REQUEST_TIMEOUT,
                "peer did not send initial replay request; closing exact RLPx session"
            );
            return;
        }
    };
    // SYSCOIN: Receiving the EN's replay request proves both endpoints kept this exact physical
    // satellite stream. Publish lifecycle before releasing its matching verifier lane.
    lifecycle.establish();
    lifecycle.activate_twofa();
    events_sender
        .send(ProtocolEvent::ReplayRequested {
            peer_id,
            starting_block: request.starting_block,
        })
        .ok();
    let max_blocks_per_message = request
        .max_blocks_per_message
        .unwrap_or(1)
        .clamp(1, MAX_BLOCKS_PER_MESSAGE) as usize;
    let max_blocks_per_message = max_blocks_per_message.min(MAX_REPLAY_RECORDS_PER_RESPONSE);
    // Overrides let a debugging EN sync reverted records that are stored under non-canonical
    // db keys (see `en_replay_record_overrides` config).
    let starting_block = request.starting_block;
    let db_key_overrides: HashMap<_, _> = request
        .record_overrides
        .into_iter()
        // SYSCOIN: Overrides behind the requested cursor can never be consumed, so do not retain
        // even bounded attacker-controlled entries for the lifetime of the peer session.
        .filter(|record_override| record_override.block_number >= starting_block)
        .map(|record_override| {
            (
                record_override.block_number,
                record_override.db_key.to_vec(),
            )
        })
        .collect();

    // Stream records to the EN indefinitely.
    let mut stream = replay
        .clone()
        .stream_from_forever(starting_block, db_key_overrides);
    loop {
        tokio::select! {
            // Biased because first branch always leads to early return. Makes sense to check it
            // first.
            biased;

            msg = conn.next() => {
                match msg {
                    Some(msg) => {
                        // SYSCOIN: Never log attacker-controlled replay override payloads; the
                        // fixed message ID is sufficient to diagnose a protocol violation.
                        tracing::info!(message_id = ?msg.message_id(), "received unexpected message from peer; terminating");
                        return;
                    }
                    None => {
                        tracing::info!("peer connection closed; terminating");
                        return;
                    }
                }
            }
            record = stream.next() => {
                let Some(record) = record else {
                    // stream_from_forever only ends if storage closes.
                    tracing::info!("replay stream closed; terminating");
                    return;
                };
                let mut records = vec![record];
                let mut replay_stream_closed = false;
                while records.len() < max_blocks_per_message {
                    match stream.next().now_or_never() {
                        Some(Some(record)) => records.push(record),
                        Some(None) => {
                            replay_stream_closed = true;
                            break;
                        }
                        None => break,
                    }
                }
                let block_numbers: Vec<_> = records
                    .iter()
                    .map(|record| record.block_context.block_number)
                    .collect();
                // SYSCOIN: Limit only replay frames in the outbound queue. Control traffic keeps
                // the general channel capacity, while slow peers cannot prebuffer many large
                // replay responses.
                let Ok(replay_queue_permit) = replay_queue_permits.clone().acquire_owned().await
                else {
                    return;
                };
                // SYSCOIN: Wait for outbound buffer capacity before encoding the full replay
                // response. Slow peers must not retain an extra pending encoded frame.
                let Ok(permit) = outbound_tx.reserve().await else {
                    return;
                };
                let encoded = ZksMessage::<P>::block_replays(records).encoded();
                permit.send(OutboundMessage::replay(encoded, replay_queue_permit));
                for block_number in block_numbers {
                    events_sender
                        .send(ProtocolEvent::ReplayBlockSent {
                            peer_id,
                            block_number,
                        })
                        .ok();
                }
                if replay_stream_closed {
                    tracing::info!("replay stream closed; terminating");
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{HandlerSharedState, ZksConnection};
    use crate::version::ZksProtocolV5;
    use alloy::primitives::{B256, BlockNumber};
    use futures::{StreamExt, stream};
    use reth_network::Direction;
    use std::collections::HashSet;
    use std::net::SocketAddr;
    use zksync_os_storage_api::{BlockContext, ReplayRecord};

    #[derive(Debug, Clone, Copy)]
    struct EmptyReplay;

    impl ReadReplay for EmptyReplay {
        // SYSCOIN: Empty test storage has no persisted rebuild/durability evidence.
        fn get_original_context(&self, _block_number: BlockNumber) -> Option<BlockContext> {
            None
        }

        fn get_replay_record_identity(&self, _block_number: BlockNumber) -> Option<B256> {
            None
        }

        fn get_context(&self, _block_number: BlockNumber) -> Option<BlockContext> {
            None
        }

        fn get_replay_record_by_key(
            &self,
            _block_number: BlockNumber,
            _db_key: Option<Vec<u8>>,
        ) -> Option<ReplayRecord> {
            None
        }

        fn get_canonical_block_hash(&self, _block_number: BlockNumber) -> Option<B256> {
            None
        }

        fn latest_record(&self) -> BlockNumber {
            0
        }
    }

    // SYSCOIN: A peer that passes RLPx/capability admission but never sends GetBlockReplays must
    // close its exact mandatory wrapper, releasing both replay ownership and the admission slot.
    #[tokio::test(start_paused = true)]
    async fn silent_peer_releases_replay_admission_after_initial_request_timeout() {
        let (events_tx, _events_rx) = mpsc::unbounded_channel();
        let state = HandlerSharedState::new(events_tx, 1, HashSet::new());
        let peer_id = PeerId::repeat_byte(0xB1);
        let remote_addr: SocketAddr = "127.0.0.1:30307".parse().unwrap();
        let permit = state.try_acquire_connection_slot().unwrap();
        let token = state.try_claim_connection(peer_id).unwrap();
        let lifecycle = ReplayConnectionLifecycle::new(
            state.clone(),
            Direction::Incoming,
            peer_id,
            remote_addr,
            token,
        );
        let (outbound_tx, outbound_rx) = mpsc::channel(1);
        let task = tokio::spawn(async move {
            let mut lifecycle = lifecycle;
            run_mn_connection::<ZksProtocolV5, _>(
                stream::pending(),
                outbound_tx,
                Arc::new(Semaphore::new(1)),
                lifecycle.events_sender(),
                peer_id,
                EmptyReplay,
                &mut lifecycle,
            )
            .await;
            assert!(!lifecycle.is_established());
        });
        let mut connection = ZksConnection {
            outbound_rx,
            task: Some(task),
            _permit: Some(permit),
        };
        assert_eq!(state.active_connections(), 1);

        tokio::task::yield_now().await;
        tokio::time::advance(INITIAL_REPLAY_REQUEST_TIMEOUT).await;
        assert!(connection.next().await.is_none());
        drop(connection);

        assert_eq!(state.active_connections(), 0);
        let replacement = state
            .try_claim_connection(peer_id)
            .expect("timed-out replay owner must be released");
        assert!(state.finish_connection_if_owner(peer_id, replacement, false));
    }
}
