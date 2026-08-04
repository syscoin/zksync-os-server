use super::MAX_BLOCKS_PER_MESSAGE;
use super::ProtocolEvent;
use super::connection::OutboundMessage;
use crate::version::ZksProtocolVersionSpec;
use crate::wire::message::ZksMessage;
use futures::{FutureExt, Stream, StreamExt};
use reth_network_peers::PeerId;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Semaphore, mpsc};
use zksync_os_storage_api::{ReadReplay, ReadReplayExt};

// SYSCOIN: Main-node replay responses may contain large records, so do not aggregate multiple
// records into one outbound frame before the network has applied backpressure.
const MAX_REPLAY_RECORDS_PER_RESPONSE: usize = 1;

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
) {
    // Receive the single GetBlockReplays request for this connection.
    let request = match conn.next().await {
        Some(ZksMessage::GetBlockReplays(request)) => request,
        Some(msg) => {
            tracing::info!(
                ?msg,
                "received unexpected initial message from peer; terminating"
            );
            return;
        }
        None => return,
    };
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
                        tracing::info!(?msg, "received unexpected message from peer; terminating");
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
