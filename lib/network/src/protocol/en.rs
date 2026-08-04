use super::MAX_BLOCKS_PER_MESSAGE;
use super::config::ExternalNodeProtocolConfig;
use super::connection::OutboundMessage;
use crate::version::ZksProtocolVersionSpec;
use crate::wire::message::ZksMessage;
use crate::wire::replays::{RecordOverride, WireReplayRecord};
use alloy::primitives::BlockNumber;
use futures::{Stream, StreamExt};
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;
use zksync_os_storage_api::ReplayRecord;

/// Background task that drives the external-node side of a `zks` connection.
///
/// Sends a `GetBlockReplays` request immediately, then forwards each received `BlockReplays`
/// record to the local sequencer via `replay_sender` and advances `starting_block`.
pub(super) async fn run_en_connection<P: ZksProtocolVersionSpec>(
    conn: impl Stream<Item = ZksMessage<P>> + Unpin,
    outbound_tx: mpsc::Sender<OutboundMessage>,
    peer_id: reth_network_peers::PeerId,
    config: ExternalNodeProtocolConfig,
) {
    let ExternalNodeProtocolConfig {
        starting_block,
        record_overrides,
        max_blocks_per_message,
        trusted_main_node_peers,
        replay_sender,
        verification: _,
    } = config;

    // SYSCOIN: Only a configured main-node enode may feed replay records to an EN. The RLPx
    // handshake authenticates `peer_id`; the allowlist authorizes that identity as a replay source.
    if !trusted_main_node_peers.contains(&peer_id) {
        tracing::warn!(
            %peer_id,
            trusted_main_node_peers = ?trusted_main_node_peers,
            "terminating replay connection from untrusted peer"
        );
        return;
    }

    if send_replay_request::<P>(
        &outbound_tx,
        &starting_block,
        record_overrides,
        max_blocks_per_message,
    )
    .await
    .is_err()
    {
        return;
    }
    receive_replays(conn, starting_block, replay_sender).await;
}

async fn send_replay_request<P: ZksProtocolVersionSpec>(
    outbound_tx: &mpsc::Sender<OutboundMessage>,
    starting_block: &Arc<RwLock<BlockNumber>>,
    record_overrides: Vec<RecordOverride>,
    max_blocks_per_message: u64,
) -> Result<(), ()> {
    let next_block = *starting_block.read().unwrap();
    tracing::info!(next_block, "requesting block replays from main node");
    // The field remains optional to preserve the published `zks/5` replay-request encoding.
    // `None` makes the main node fall back to one record per response.
    let max_blocks_per_message = Some(max_blocks_per_message.clamp(1, MAX_BLOCKS_PER_MESSAGE));
    let msg =
        ZksMessage::<P>::get_block_replays(next_block, max_blocks_per_message, record_overrides);
    outbound_tx
        .send(OutboundMessage::control(msg.encoded()))
        .await
        .map_err(|_| ())
}

async fn receive_replays<P: ZksProtocolVersionSpec>(
    mut conn: impl Stream<Item = ZksMessage<P>> + Unpin,
    starting_block: Arc<RwLock<BlockNumber>>,
    replay_sender: mpsc::Sender<ReplayRecord>,
) {
    while let Some(msg) = conn.next().await {
        match msg {
            ZksMessage::GetBlockReplays(_) => {
                tracing::info!("ignoring request as local node is also waiting for records");
            }
            ZksMessage::BlockReplays(response) => {
                for record in response.records {
                    let block_number = record.block_number();
                    tracing::debug!(block_number, "received block replay");
                    let record: ReplayRecord = match record.try_into() {
                        Ok(record) => record,
                        Err(error) => {
                            tracing::info!(%error, "failed to recover replay block");
                            return;
                        }
                    };

                    let expected_next_block = *starting_block.read().unwrap();
                    assert_eq!(block_number, expected_next_block);

                    if replay_sender.send(record).await.is_err() {
                        tracing::trace!("network replay channel is closed");
                        return;
                    }
                    *starting_block.write().unwrap() += 1;
                }
            }
        }
    }
}
