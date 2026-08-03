use super::MAX_BLOCKS_PER_MESSAGE;
use super::config::{ExternalNodeProtocolConfig, ExternalNodeVerifierConfig};
use super::connection::OutboundMessage;
use crate::service::{PeerVerifyBatch, PeerVerifyBatchResult};
use crate::version::ZksProtocolVersionSpec;
use crate::wire::message::ZksMessage;
use crate::wire::replays::{RecordOverride, WireReplayRecord};
use alloy::primitives::BlockNumber;
use alloy::signers::{SignerSync, local::PrivateKeySigner};
use futures::{Stream, StreamExt};
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;
use zksync_os_storage_api::ReplayRecord;

/// Background task that drives the external-node side of a `zks` connection.
///
/// Sends a `GetBlockReplays` request immediately, then forwards each received `BlockReplays`
/// record to the local sequencer via `replay_sender` and advances `starting_block`.
pub(super) async fn run_en_connection<P: ZksProtocolVersionSpec>(
    mut conn: impl Stream<Item = ZksMessage<P>> + Unpin,
    outbound_tx: mpsc::Sender<OutboundMessage>,
    peer_id: PeerId,
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
    // SYSCOIN: Only the configured main-node enode is allowed to feed replay records to an EN.
    if !trusted_main_node_peers.contains(&peer_id) {
        tracing::warn!(
            %peer_id,
            trusted_main_node_peers = ?trusted_main_node_peers,
            "terminating replay connection from untrusted peer"
        );
        return;
    }

    if perform_verifier_handshake::<P>(&mut conn, &outbound_tx, verifier.as_ref())
        .await
        .is_err()
    {
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
    receive_replay_and_verification(
        conn,
        outbound_tx,
        starting_block,
        replay_sender,
        peer_id,
        verifier,
    )
    .await;
}

async fn perform_verifier_handshake<P: ZksProtocolVersionSpec>(
    conn: &mut (impl Stream<Item = ZksMessage<P>> + Unpin),
    outbound_tx: &mpsc::Sender<OutboundMessage>,
    verifier: Option<&ExternalNodeVerifierConfig>,
) -> Result<(), ()> {
    let Some(verifier) = verifier else {
        return Ok(());
    };
    if !P::VERSION.supports_message(ZksMessageId::VerifierRoleRequest) {
        return Ok(());
    }

    let msg = ZksMessage::<P>::VerifierRoleRequest(Default::default());
    if outbound_tx
        .send(OutboundMessage::control(msg.encoded()))
        .await
        .is_err()
    {
        return Err(());
    }

    let signer = match PrivateKeySigner::from_str(verifier.signing_key.expose_secret()) {
        Ok(signer) => signer,
        Err(error) => {
            tracing::info!(%error, "invalid verifier signing key; terminating");
            return Err(());
        }
    };

    let challenge = match conn.next().await {
        Some(ZksMessage::VerifierChallenge(challenge)) => challenge,
        Some(other) => {
            tracing::info!(
                ?other,
                "received unexpected message while waiting for verifier challenge; terminating"
            );
            return Err(());
        }
        None => return Err(()),
    };

    let signature = match signer.sign_hash_sync(&verifier_auth_prehash(challenge.nonce)) {
        Ok(signature) => signature,
        Err(error) => {
            tracing::info!(%error, "failed to sign verifier challenge; terminating");
            return Err(());
        }
    };

    let msg = ZksMessage::<P>::VerifierAuth(VerifierAuth {
        signature: signature.as_bytes().to_vec().into(),
    });
    if outbound_tx
        .send(OutboundMessage::control(msg.encoded()))
        .await
        .is_err()
    {
        return Err(());
    }
    Ok(())
}

async fn send_replay_request<P: ZksProtocolVersionSpec>(
    outbound_tx: &mpsc::Sender<OutboundMessage>,
    starting_block: &Arc<RwLock<BlockNumber>>,
    record_overrides: Vec<RecordOverride>,
    max_blocks_per_message: u64,
) -> Result<(), ()> {
    let next_block = *starting_block.read().unwrap();
    tracing::info!(next_block, "requesting block replays from main node");
    // The field remains optional to preserve the published `zks/5` request encoding. `None` is
    // valid on the wire and makes the main node fall back to one record per response.
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
    outbound_tx: mpsc::Sender<OutboundMessage>,
    starting_block: Arc<RwLock<BlockNumber>>,
    replay_sender: mpsc::Sender<ReplayRecord>,
) {
    while let Some(msg) = conn.next().await {
        match msg {
            ZksMessage::GetBlockReplays(_) => {
                tracing::info!("ignoring request as local node is also waiting for records");
            }
            result = recv_outgoing_verify_result(&mut outgoing_verify_results) => {
                let Some(result) = result else {
                    continue;
                };
                if result.peer_id != peer_id {
                    continue;
                }
                if outbound_tx
                    .send(OutboundMessage::control(
                        ZksMessage::<P>::VerifyBatchResult(result.message).encoded(),
                    ))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }
    }
}

