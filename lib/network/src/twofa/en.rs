use super::config::ExternalNode2faConfig;
use super::protocol::{Zks2faPeerHandle, wait_for_lane_cancellation};
use super::wire::Zks2faMessage;
use crate::service::{PeerVerifyBatch, PeerVerifyBatchResult};
use crate::wire::auth::{VerifierAuth, verifier_auth_prehash};
use crate::wire::verification::{VerifyBatchOutcome, bounded_verify_batch_refusal_reason};
use alloy::primitives::bytes::BytesMut;
use alloy::signers::{SignerSync, local::PrivateKeySigner};
use futures::{Stream, StreamExt};
use reth_network_peers::PeerId;
use secrecy::ExposeSecret;
use std::str::FromStr;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, watch};

// SYSCOIN: A verifier must not retain its lane indefinitely while a silent main node withholds
// the challenge. This matches the main-node authentication-step deadline.
const VERIFIER_AUTH_STEP_TIMEOUT: Duration = Duration::from_secs(10);

// SYSCOIN: Every worker exit closes its shared lane state and wakes registry aliases.
struct CloseLaneOnDrop(Zks2faPeerHandle);

impl Drop for CloseLaneOnDrop {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// Background task that drives the external-node side of a `zks_2fa` connection.
///
/// Performs the verifier handshake immediately, then forwards each received `VerifyBatch` request
/// into the local verifier via `verify_batch_tx` and relays signed `VerifyBatchResult`s back to the
/// requesting main node.
#[cfg(test)]
pub(super) async fn run_2fa_en_connection(
    mut conn: impl Stream<Item = Zks2faMessage> + Unpin,
    outbound_tx: mpsc::Sender<BytesMut>,
    peer_id: PeerId,
    config: ExternalNode2faConfig,
    peer_handle: Zks2faPeerHandle,
) {
    drive_2fa_en_connection(&mut conn, outbound_tx, peer_id, config, peer_handle, || {
        Some(())
    })
    .await;
}

// SYSCOIN: Production keeps ownership of the typed inbound stream in the protocol supervisor so
// replay-preserving worker exits can release their lane and then actively drain the same stream.
// The shared lane is registered only after the MN answers on this exact 2FA stream.
pub(super) async fn drive_2fa_en_connection<Registration>(
    conn: &mut (impl Stream<Item = Zks2faMessage> + Unpin),
    outbound_tx: mpsc::Sender<BytesMut>,
    peer_id: PeerId,
    config: ExternalNode2faConfig,
    peer_handle: Zks2faPeerHandle,
    register_after_challenge: impl FnOnce() -> Option<Registration>,
) {
    // SYSCOIN: Channel saturation, handshake failure, and protocol violations all close the
    // connection-local state before the wrapper releases the lane permit.
    let _close_lane = CloseLaneOnDrop(peer_handle.clone());
    // SYSCOIN: The authenticated RLPx identity is checked before subscribing, signing, sending a
    // role frame, reading peer data, or admitting local verifier work. Empty means deny all.
    if !config.trusted_main_node_peers.contains(&peer_id) {
        tracing::warn!(%peer_id, "rejecting untrusted zks_2fa main-node peer");
        return;
    }
    let mut cancellation_rx = peer_handle.cancellation_receiver();
    // Subscribe before the handshake so a result broadcast right after authentication is not
    // missed (a `broadcast::Receiver` only observes messages sent after it subscribed).
    let outgoing_verify_results = config.outgoing_verify_results.subscribe();
    let Ok(_registration) = perform_verifier_handshake(
        conn,
        &outbound_tx,
        peer_id,
        &config,
        &peer_handle,
        &mut cancellation_rx,
        register_after_challenge,
    )
    .await
    else {
        return;
    };
    receive_verification(
        conn,
        outbound_tx,
        peer_id,
        &peer_handle,
        config,
        outgoing_verify_results,
        &mut cancellation_rx,
    )
    .await;
}

// SYSCOIN: Drive the EN's connection-local role/auth transition before registering a live lane.
async fn perform_verifier_handshake<Registration>(
    conn: &mut (impl Stream<Item = Zks2faMessage> + Unpin),
    outbound_tx: &mpsc::Sender<BytesMut>,
    main_node_peer_id: PeerId,
    config: &ExternalNode2faConfig,
    peer_handle: &Zks2faPeerHandle,
    cancellation_rx: &mut watch::Receiver<bool>,
    register_after_challenge: impl FnOnce() -> Option<Registration>,
) -> Result<Registration, ()> {
    if !peer_handle.begin_authentication() {
        tracing::warn!(%main_node_peer_id, "verifier lane rejected role transition");
        return Err(());
    }
    // SYSCOIN: Handshake frames use nonblocking bounded admission; a wedged outbound lane is
    // terminal rather than retaining a connection slot indefinitely.
    match outbound_tx.try_send(Zks2faMessage::verifier_role_request().encoded()) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(_)) => {
            // SYSCOIN: A transiently wedged writer must renegotiate the optional capability;
            // parking this first handshake forever would permanently remove verifier capacity.
            peer_handle.close_for_session_recovery();
            tracing::warn!(%main_node_peer_id, channel_state = "full", "failed to enqueue verifier role request");
            return Err(());
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            tracing::warn!(%main_node_peer_id, channel_state = "closed", "failed to enqueue verifier role request");
            return Err(());
        }
    }
    let signer = match PrivateKeySigner::from_str(config.signing_key.expose_secret()) {
        Ok(signer) => signer,
        Err(error) => {
            tracing::info!(%error, "invalid verifier signing key; terminating");
            return Err(());
        }
    };

    // SYSCOIN: The challenge phase is cancellation-aware and has a fixed authentication bound.
    let challenge = match tokio::select! {
        biased;
        _ = wait_for_lane_cancellation(cancellation_rx) => return Err(()),
        challenge = tokio::time::timeout(VERIFIER_AUTH_STEP_TIMEOUT, conn.next()) => challenge,
    } {
        Ok(Some(Zks2faMessage::VerifierChallenge(challenge))) => challenge,
        Ok(Some(other)) => {
            tracing::info!(
                message_id = ?other.message_id(),
                "received unexpected message while waiting for verifier challenge; terminating"
            );
            peer_handle.close_for_session_recovery();
            return Err(());
        }
        Ok(None) => return Err(()),
        Err(_) => {
            // SYSCOIN: Capability negotiation only runs on a fresh RLPx session, so a transient
            // challenge timeout must restart the owning session instead of leaving an inert lane.
            peer_handle.close_for_session_recovery();
            tracing::warn!(%main_node_peer_id, "verifier challenge timed out; terminating");
            return Err(());
        }
    };

    // SYSCOIN: A challenge proves the remote MN retained this exact 2FA stream after replay's
    // role-specific gate. Only now may the EN publish a shared registry handle; crossed dials and
    // silent tentative lanes leave zero registry state.
    let Some(registration) = register_after_challenge() else {
        peer_handle.close_for_session_recovery();
        tracing::warn!(%main_node_peer_id, "failed to claim mutually proven verifier lane");
        return Err(());
    };

    let signature = match signer.sign_hash_sync(&verifier_auth_prehash(
        config.chain_id,
        main_node_peer_id,
        config.local_peer_id,
        challenge.nonce,
    )) {
        Ok(signature) => signature,
        Err(error) => {
            tracing::info!(%error, "failed to sign verifier challenge; terminating");
            return Err(());
        }
    };

    let msg = Zks2faMessage::VerifierAuth(VerifierAuth {
        signature: signature.as_bytes().to_vec().into(),
    });
    // SYSCOIN: Authorization and bounded auth-frame admission belong to this exact lane.
    if !peer_handle.authorize() {
        tracing::warn!(%main_node_peer_id, "verifier lane rejected auth transition");
        return Err(());
    }
    match outbound_tx.try_send(msg.encoded()) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(_)) => {
            // SYSCOIN: The signed handshake cannot be retried on this transcript after writer
            // saturation; restart RLPx to obtain a new nonce and exact lane generation.
            peer_handle.close_for_session_recovery();
            tracing::warn!(%main_node_peer_id, channel_state = "full", "failed to enqueue verifier auth");
            return Err(());
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            tracing::warn!(%main_node_peer_id, channel_state = "closed", "failed to enqueue verifier auth");
            return Err(());
        }
    }
    Ok(registration)
}

// SYSCOIN: Cancellation and inbound verification traffic share one bounded post-auth receive loop.
async fn receive_verification(
    mut conn: impl Stream<Item = Zks2faMessage> + Unpin,
    outbound_tx: mpsc::Sender<BytesMut>,
    peer_id: PeerId,
    peer_handle: &Zks2faPeerHandle,
    config: ExternalNode2faConfig,
    mut outgoing_verify_results: broadcast::Receiver<PeerVerifyBatchResult>,
    cancellation_rx: &mut watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            biased;
            _ = wait_for_lane_cancellation(cancellation_rx) => break,
            msg = conn.next() => {
                let Some(msg) = msg else {
                    break;
                };
                match msg {
                    Zks2faMessage::VerifyBatch(request) => {
                        // SYSCOIN: Never await a saturated local verifier queue; fail this lane
                        // closed so one main-node peer cannot pin the EN connection task.
                        match config.verify_batch_tx.try_send(PeerVerifyBatch {
                                peer_id,
                                lane_id: peer_handle.lane_id(),
                                message: request,
                            }) {
                            Ok(()) => {}
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                // SYSCOIN: An admitted request that cannot reach the bounded local
                                // verifier must reset immediately so the MN can retry another lane.
                                peer_handle.close_for_session_recovery();
                                tracing::warn!(%peer_id, "verify batch channel is full; terminating");
                                break;
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => {
                                tracing::info!(%peer_id, "verify batch channel is closed; terminating");
                                break;
                            }
                        }
                    }
                    other => {
                        // SYSCOIN: Post-auth protocol violations are terminal, avoiding repeated
                        // malformed traffic on a capped verifier lane.
                        tracing::warn!(message_id = ?other.message_id(), "unexpected zks_2fa message; terminating");
                        peer_handle.close_for_session_recovery();
                        break;
                    }
                }
            }
            result = recv_outgoing_verify_result(&mut outgoing_verify_results) => {
                let Some(mut result) = result else {
                    break;
                };
                // SYSCOIN: Require exact connection-generation ownership. A delayed local result
                // from a superseded lane must never be forwarded by its same-peer replacement.
                if result.peer_id != peer_id || result.lane_id != peer_handle.lane_id() {
                    continue;
                }
                // SYSCOIN: Enforce the shared byte-exact refusal contract again at the final wire
                // boundary so an alternate local producer cannot emit an oversized result frame.
                if let VerifyBatchOutcome::Refused(reason) = &mut result.message.result {
                    let original_bytes = reason.len();
                    *reason = bounded_verify_batch_refusal_reason(std::mem::take(reason));
                    if reason.len() != original_bytes {
                        tracing::warn!(
                            %peer_id,
                            original_bytes,
                            transmitted_bytes = reason.len(),
                            "bounded local verifier refusal before transmission"
                        );
                    }
                }
                // SYSCOIN: A slow RLPx writer must not block local verification admission.
                match outbound_tx.try_send(Zks2faMessage::VerifyBatchResult(result.message).encoded()) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        // SYSCOIN: Once local work produced an exact result, writer saturation
                        // cannot be repaired on this request generation; force prompt redial.
                        peer_handle.close_for_session_recovery();
                        tracing::warn!(%peer_id, channel_state = "full", "failed to enqueue verify result; terminating");
                        break;
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        tracing::warn!(%peer_id, channel_state = "closed", "failed to enqueue verify result; terminating");
                        break;
                    }
                }
            }
        }
    }
}

async fn recv_outgoing_verify_result(
    receiver: &mut broadcast::Receiver<PeerVerifyBatchResult>,
) -> Option<PeerVerifyBatchResult> {
    loop {
        match receiver.recv().await {
            Ok(result) => return Some(result),
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(skipped, "lagged on outgoing verify results broadcast");
            }
            Err(broadcast::error::RecvError::Closed) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::twofa::protocol::VerifyDispatchError;
    use crate::wire::verification::{VerifyBatch, VerifyBatchOutcome, VerifyBatchResult};
    use alloy::primitives::{B256, Bytes};
    use futures::{channel::mpsc as futures_mpsc, stream};
    use secrecy::SecretString;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    const TEST_SIGNING_KEY: &str =
        "0x7726827caac94a7f9e1b160f7ea819f172f7b6f9d2a97f992c38edeab82d4110";
    const TEST_CHAIN_ID: u64 = 57_057;

    fn main_node_peer_id() -> PeerId {
        PeerId::repeat_byte(0x11)
    }

    fn verifier_peer_id() -> PeerId {
        PeerId::repeat_byte(0x22)
    }

    fn verify_request(request_id: u64) -> VerifyBatch {
        VerifyBatch {
            request_id,
            batch_number: 7,
            first_block_number: 100,
            last_block_number: 120,
            pubdata_mode: 0,
            commit_data: Bytes::from_static(b"commit"),
            prev_commit_data: Bytes::from_static(b"prev"),
            execution_protocol_version: 32,
        }
    }

    fn config(
        verify_batch_tx: mpsc::Sender<PeerVerifyBatch>,
        outgoing_verify_results: broadcast::Sender<PeerVerifyBatchResult>,
    ) -> ExternalNode2faConfig {
        ExternalNode2faConfig {
            chain_id: TEST_CHAIN_ID,
            local_peer_id: verifier_peer_id(),
            trusted_main_node_peers: vec![main_node_peer_id()],
            signing_key: SecretString::from(TEST_SIGNING_KEY),
            verify_batch_tx,
            outgoing_verify_results,
        }
    }

    // SYSCOIN: An RLPx peer outside the configured boot-node identities cannot trigger even the
    // first auth frame or enqueue local verification work.
    #[tokio::test]
    async fn untrusted_main_node_is_rejected_before_frames_or_work() {
        let untrusted_peer_id = PeerId::repeat_byte(0x77);
        let (verify_batch_tx, mut verify_batch_rx) = mpsc::channel(1);
        let (outgoing_verify_results, _outgoing_verify_results_rx) = broadcast::channel(1);
        let (outbound_tx, mut outbound_rx) = mpsc::channel(8);
        let peer_handle = Zks2faPeerHandle::new(outbound_tx.clone());

        run_2fa_en_connection(
            stream::iter([
                Zks2faMessage::verifier_challenge(B256::repeat_byte(0x44)),
                Zks2faMessage::VerifyBatch(verify_request(2)),
            ]),
            outbound_tx,
            untrusted_peer_id,
            config(verify_batch_tx, outgoing_verify_results),
            peer_handle.clone(),
        )
        .await;

        assert!(outbound_rx.try_recv().is_err());
        assert!(verify_batch_rx.try_recv().is_err());
        assert!(!peer_handle.is_open());
        assert!(!peer_handle.requires_session_disconnect());
    }

    // SYSCOIN: Empty is deny-all rather than an accidental wildcard for verifier work.
    #[tokio::test]
    async fn empty_trusted_main_node_set_rejects_every_peer() {
        let (verify_batch_tx, mut verify_batch_rx) = mpsc::channel(1);
        let (outgoing_verify_results, _outgoing_verify_results_rx) = broadcast::channel(1);
        let (outbound_tx, mut outbound_rx) = mpsc::channel(8);
        let peer_handle = Zks2faPeerHandle::new(outbound_tx.clone());
        let mut config = config(verify_batch_tx, outgoing_verify_results);
        config.trusted_main_node_peers.clear();

        run_2fa_en_connection(
            stream::iter([
                Zks2faMessage::verifier_challenge(B256::repeat_byte(0x44)),
                Zks2faMessage::VerifyBatch(verify_request(2)),
            ]),
            outbound_tx,
            main_node_peer_id(),
            config,
            peer_handle.clone(),
        )
        .await;

        assert!(outbound_rx.try_recv().is_err());
        assert!(verify_batch_rx.try_recv().is_err());
        assert!(!peer_handle.is_open());
    }

    #[tokio::test]
    async fn local_verifier_request_is_scoped_to_exact_lane() {
        let request = verify_request(2);
        let (verify_batch_tx, mut verify_batch_rx) = mpsc::channel(1);
        let (outgoing_verify_results, outgoing_verify_results_rx) = broadcast::channel(1);
        let (outbound_tx, _outbound_rx) = mpsc::channel(1);
        let peer_handle = Zks2faPeerHandle::new(outbound_tx.clone());
        let (_cancellation_tx, mut cancellation_rx) = watch::channel(false);

        receive_verification(
            stream::iter([Zks2faMessage::VerifyBatch(request.clone())]),
            outbound_tx,
            main_node_peer_id(),
            &peer_handle,
            config(verify_batch_tx, outgoing_verify_results),
            outgoing_verify_results_rx,
            &mut cancellation_rx,
        )
        .await;

        let queued = verify_batch_rx.try_recv().unwrap();
        assert_eq!(queued.peer_id, main_node_peer_id());
        assert_eq!(queued.lane_id, peer_handle.lane_id());
        assert_eq!(queued.message, request);
    }

    #[tokio::test]
    async fn full_local_verify_queue_is_nonblocking_and_terminal() {
        let (verify_batch_tx, mut verify_batch_rx) = mpsc::channel(1);
        let retained = PeerVerifyBatch {
            peer_id: PeerId::repeat_byte(0x99),
            lane_id: 99,
            message: verify_request(1),
        };
        verify_batch_tx.try_send(retained.clone()).unwrap();
        let (outgoing_verify_results, outgoing_verify_results_rx) = broadcast::channel(1);
        let (outbound_tx, _outbound_rx) = mpsc::channel(1);
        let peer_handle = Zks2faPeerHandle::new(outbound_tx.clone());
        let (_cancellation_tx, mut cancellation_rx) = watch::channel(false);

        tokio::time::timeout(
            Duration::from_millis(100),
            receive_verification(
                stream::iter([Zks2faMessage::VerifyBatch(verify_request(2))]),
                outbound_tx,
                main_node_peer_id(),
                &peer_handle,
                config(verify_batch_tx, outgoing_verify_results),
                outgoing_verify_results_rx,
                &mut cancellation_rx,
            ),
        )
        .await
        .expect("a full local verifier queue must fail closed without waiting");

        let queued = verify_batch_rx.try_recv().unwrap();
        assert_eq!(queued.peer_id, retained.peer_id);
        assert_eq!(queued.message, retained.message);
        assert!(verify_batch_rx.try_recv().is_err());
        assert!(peer_handle.requires_session_disconnect());
    }

    #[tokio::test]
    async fn closed_local_verify_queue_is_terminal() {
        let (verify_batch_tx, verify_batch_rx) = mpsc::channel(1);
        drop(verify_batch_rx);
        let (outgoing_verify_results, outgoing_verify_results_rx) = broadcast::channel(1);
        let (outbound_tx, _outbound_rx) = mpsc::channel(1);
        let peer_handle = Zks2faPeerHandle::new(outbound_tx.clone());
        let (_cancellation_tx, mut cancellation_rx) = watch::channel(false);

        tokio::time::timeout(
            Duration::from_millis(100),
            receive_verification(
                stream::iter([Zks2faMessage::VerifyBatch(verify_request(2))]),
                outbound_tx,
                main_node_peer_id(),
                &peer_handle,
                config(verify_batch_tx, outgoing_verify_results),
                outgoing_verify_results_rx,
                &mut cancellation_rx,
            ),
        )
        .await
        .expect("a closed local verifier queue must terminate the lane");
        assert!(!peer_handle.requires_session_disconnect());
    }

    #[tokio::test]
    async fn unexpected_post_auth_message_marks_exact_session_for_disconnect() {
        let (verify_batch_tx, _verify_batch_rx) = mpsc::channel(1);
        let (outgoing_verify_results, outgoing_verify_results_rx) = broadcast::channel(1);
        let (outbound_tx, _outbound_rx) = mpsc::channel(1);
        let peer_handle = Zks2faPeerHandle::new(outbound_tx.clone());
        let (_cancellation_tx, mut cancellation_rx) = watch::channel(false);

        receive_verification(
            stream::iter([Zks2faMessage::verifier_challenge(B256::repeat_byte(0x44))]),
            outbound_tx,
            main_node_peer_id(),
            &peer_handle,
            config(verify_batch_tx, outgoing_verify_results),
            outgoing_verify_results_rx,
            &mut cancellation_rx,
        )
        .await;

        assert!(peer_handle.requires_session_disconnect());
    }

    #[tokio::test]
    async fn unexpected_handshake_message_marks_exact_session_for_disconnect() {
        let (outbound_tx, _outbound_rx) = mpsc::channel(8);
        let (verify_batch_tx, _verify_batch_rx) = mpsc::channel(1);
        let (outgoing_verify_results, _outgoing_verify_results_rx) = broadcast::channel(1);
        let peer_handle = Zks2faPeerHandle::new(outbound_tx.clone());

        run_2fa_en_connection(
            stream::iter([Zks2faMessage::verifier_role_request()]),
            outbound_tx,
            main_node_peer_id(),
            config(verify_batch_tx, outgoing_verify_results),
            peer_handle.clone(),
        )
        .await;

        assert!(peer_handle.requires_session_disconnect());
    }

    #[tokio::test]
    async fn replacement_ignores_delayed_result_from_superseded_lane() {
        let (inbound_tx, inbound_rx) = futures_mpsc::unbounded();
        let (outbound_tx, mut outbound_rx) = mpsc::channel(2);
        let peer_handle = Zks2faPeerHandle::new(outbound_tx.clone());
        let new_lane_id = peer_handle.lane_id();
        let old_lane_id = new_lane_id.wrapping_add(1);
        let (verify_batch_tx, _verify_batch_rx) = mpsc::channel(1);
        let (outgoing_verify_results, outgoing_verify_results_rx) = broadcast::channel(2);
        let results_tx = outgoing_verify_results.clone();
        let (_cancellation_tx, mut cancellation_rx) = watch::channel(false);
        let task = tokio::spawn(async move {
            receive_verification(
                inbound_rx,
                outbound_tx,
                main_node_peer_id(),
                &peer_handle,
                config(verify_batch_tx, outgoing_verify_results),
                outgoing_verify_results_rx,
                &mut cancellation_rx,
            )
            .await;
        });

        let old_result = VerifyBatchResult {
            request_id: 5,
            batch_number: 7,
            result: VerifyBatchOutcome::Refused("old lane".to_owned()),
        };
        let new_result = VerifyBatchResult {
            request_id: 6,
            batch_number: 8,
            result: VerifyBatchOutcome::Refused("replacement lane".to_owned()),
        };
        results_tx
            .send(PeerVerifyBatchResult {
                peer_id: main_node_peer_id(),
                lane_id: old_lane_id,
                message: old_result,
            })
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(25), outbound_rx.recv())
                .await
                .is_err(),
            "a replacement must not forward a delayed result owned by the old lane"
        );

        results_tx
            .send(PeerVerifyBatchResult {
                peer_id: main_node_peer_id(),
                lane_id: new_lane_id,
                message: new_result.clone(),
            })
            .unwrap();
        let encoded = tokio::time::timeout(Duration::from_millis(100), outbound_rx.recv())
            .await
            .expect("the replacement lane remains responsive")
            .expect("replacement outbound channel remains open");
        let mut encoded = encoded.as_ref();
        assert_eq!(
            Zks2faMessage::decode_message(&mut encoded).unwrap(),
            Zks2faMessage::VerifyBatchResult(new_result)
        );
        assert!(encoded.is_empty());

        drop(inbound_tx);
        task.await.unwrap();
    }

    // SYSCOIN: The final EN wire boundary applies the shared byte contract even if an alternate
    // local producer emits an oversized multibyte refusal.
    #[tokio::test]
    async fn oversized_local_refusal_is_utf8_bounded_before_wire() {
        let (inbound_tx, inbound_rx) = futures_mpsc::unbounded();
        let (outbound_tx, mut outbound_rx) = mpsc::channel(1);
        let peer_handle = Zks2faPeerHandle::new(outbound_tx.clone());
        let lane_id = peer_handle.lane_id();
        let (verify_batch_tx, _verify_batch_rx) = mpsc::channel(1);
        let (outgoing_verify_results, outgoing_verify_results_rx) = broadcast::channel(1);
        let results_tx = outgoing_verify_results.clone();
        let (_cancellation_tx, mut cancellation_rx) = watch::channel(false);
        let task = tokio::spawn(async move {
            receive_verification(
                inbound_rx,
                outbound_tx,
                main_node_peer_id(),
                &peer_handle,
                config(verify_batch_tx, outgoing_verify_results),
                outgoing_verify_results_rx,
                &mut cancellation_rx,
            )
            .await;
        });

        let oversized_reason = format!("{}é", "r".repeat(255));
        results_tx
            .send(PeerVerifyBatchResult {
                peer_id: main_node_peer_id(),
                lane_id,
                message: VerifyBatchResult {
                    request_id: 6,
                    batch_number: 8,
                    result: VerifyBatchOutcome::Refused(oversized_reason),
                },
            })
            .unwrap();
        let encoded = outbound_rx.recv().await.unwrap();
        let mut encoded = encoded.as_ref();
        let Zks2faMessage::VerifyBatchResult(result) =
            Zks2faMessage::decode_message(&mut encoded).unwrap()
        else {
            panic!("expected verify result");
        };
        let VerifyBatchOutcome::Refused(reason) = result.result else {
            panic!("expected refusal");
        };
        assert_eq!(reason, "r".repeat(255));

        drop(inbound_tx);
        task.await.unwrap();
    }

    // SYSCOIN: Full handshake writers require a new capability transcript, while a closed writer
    // is local teardown and must preserve the replay session without a reconnect loop.
    #[tokio::test]
    async fn role_writer_full_recovers_but_closed_writer_preserves_session_policy() {
        async fn run(closed: bool) -> Zks2faPeerHandle {
            let (outbound_tx, outbound_rx) = mpsc::channel(1);
            let mut outbound_rx = Some(outbound_rx);
            if closed {
                drop(outbound_rx.take());
            } else {
                outbound_tx
                    .try_send(BytesMut::from(&b"writer occupied"[..]))
                    .unwrap();
            }
            let (verify_batch_tx, _verify_batch_rx) = mpsc::channel(1);
            let (results_tx, _results_rx) = broadcast::channel(1);
            let config = config(verify_batch_tx, results_tx);
            let handle = Zks2faPeerHandle::new(outbound_tx.clone());
            let mut cancellation_rx = handle.cancellation_receiver();
            let mut conn = stream::pending();
            assert!(
                perform_verifier_handshake(
                    &mut conn,
                    &outbound_tx,
                    main_node_peer_id(),
                    &config,
                    &handle,
                    &mut cancellation_rx,
                    || Some(()),
                )
                .await
                .is_err()
            );
            handle
        }

        assert!(run(false).await.requires_session_disconnect());
        assert!(!run(true).await.requires_session_disconnect());
    }

    // SYSCOIN: The same Full/Closed distinction applies after the challenge: the signed auth
    // frame cannot be retried on its nonce, but local connection teardown needs no recovery flag.
    #[tokio::test]
    async fn auth_writer_full_recovers_but_closed_writer_preserves_session_policy() {
        async fn run(closed: bool) -> Zks2faPeerHandle {
            let (inbound_tx, inbound_rx) = futures_mpsc::unbounded();
            let (outbound_tx, outbound_rx) = mpsc::channel(1);
            let (verify_batch_tx, _verify_batch_rx) = mpsc::channel(1);
            let (results_tx, _results_rx) = broadcast::channel(1);
            let config = config(verify_batch_tx, results_tx);
            let handle = Zks2faPeerHandle::new(outbound_tx.clone());
            let task_handle = handle.clone();
            let task_outbound_tx = outbound_tx.clone();
            let task = tokio::spawn(async move {
                let mut conn = inbound_rx;
                let mut cancellation_rx = task_handle.cancellation_receiver();
                let result = perform_verifier_handshake(
                    &mut conn,
                    &task_outbound_tx,
                    main_node_peer_id(),
                    &config,
                    &task_handle,
                    &mut cancellation_rx,
                    || Some(()),
                )
                .await;
                (result, task_handle)
            });

            let mut outbound_rx = Some(outbound_rx);
            let encoded_role = outbound_rx
                .as_mut()
                .unwrap()
                .recv()
                .await
                .expect("role request is queued before challenge");
            let mut encoded_role = encoded_role.as_ref();
            assert!(matches!(
                Zks2faMessage::decode_message(&mut encoded_role).unwrap(),
                Zks2faMessage::VerifierRoleRequest(_)
            ));
            if closed {
                drop(outbound_rx.take());
            } else {
                outbound_tx
                    .try_send(BytesMut::from(&b"writer occupied"[..]))
                    .unwrap();
            }
            inbound_tx
                .unbounded_send(Zks2faMessage::verifier_challenge(B256::repeat_byte(0x55)))
                .unwrap();
            let (result, task_handle) = task.await.unwrap();
            assert!(result.is_err());
            task_handle
        }

        assert!(run(false).await.requires_session_disconnect());
        assert!(!run(true).await.requires_session_disconnect());
    }

    // SYSCOIN: Once exact local work has completed, a full RLPx writer cannot safely retain the
    // lane and wait for the MN deadline; immediate recovery makes retry capacity available.
    #[tokio::test]
    async fn full_result_writer_marks_exact_session_for_recovery() {
        let (verify_batch_tx, _verify_batch_rx) = mpsc::channel(1);
        let (results_tx, results_rx) = broadcast::channel(1);
        let (outbound_tx, _outbound_rx) = mpsc::channel(1);
        outbound_tx
            .try_send(BytesMut::from(&b"writer occupied"[..]))
            .unwrap();
        let handle = Zks2faPeerHandle::new(outbound_tx.clone());
        let (_cancellation_tx, mut cancellation_rx) = watch::channel(false);
        results_tx
            .send(PeerVerifyBatchResult {
                peer_id: main_node_peer_id(),
                lane_id: handle.lane_id(),
                message: VerifyBatchResult {
                    request_id: 7,
                    batch_number: 9,
                    result: VerifyBatchOutcome::Refused("refused".to_owned()),
                },
            })
            .unwrap();

        receive_verification(
            stream::pending(),
            outbound_tx,
            main_node_peer_id(),
            &handle,
            config(verify_batch_tx, results_tx),
            results_rx,
            &mut cancellation_rx,
        )
        .await;
        assert!(handle.requires_session_disconnect());
    }

    #[tokio::test(start_paused = true)]
    async fn silent_main_node_cannot_hold_awaiting_auth_slot() {
        let (_inbound_tx, inbound_rx) = futures_mpsc::unbounded();
        let (outbound_tx, mut outbound_rx) = mpsc::channel(8);
        let (verify_batch_tx, _verify_batch_rx) = mpsc::channel(1);
        let (outgoing_verify_results, _outgoing_verify_results_rx) = broadcast::channel(1);
        let peer_handle = Zks2faPeerHandle::new(outbound_tx.clone());
        let inspect_handle = peer_handle.clone();
        let registered = Arc::new(AtomicBool::new(false));
        let task_registered = registered.clone();
        let task = tokio::spawn(async move {
            let mut inbound_rx = inbound_rx;
            drive_2fa_en_connection(
                &mut inbound_rx,
                outbound_tx,
                main_node_peer_id(),
                config(verify_batch_tx, outgoing_verify_results),
                peer_handle,
                move || {
                    task_registered.store(true, Ordering::SeqCst);
                    Some(())
                },
            )
            .await;
        });

        let encoded_role = outbound_rx.recv().await.expect("verifier sends its role");
        let mut encoded_role = encoded_role.as_ref();
        assert!(matches!(
            Zks2faMessage::decode_message(&mut encoded_role).unwrap(),
            Zks2faMessage::VerifierRoleRequest(_)
        ));
        assert!(encoded_role.is_empty());
        tokio::time::advance(VERIFIER_AUTH_STEP_TIMEOUT).await;
        task.await.unwrap();
        assert!(
            !registered.load(Ordering::SeqCst),
            "a silent/crossed lane must never publish shared registry ownership"
        );

        assert_eq!(
            inspect_handle.try_enqueue_verify_batch(
                41,
                7,
                BytesMut::from(&b"request"[..]),
                tokio::time::Instant::now() + Duration::from_secs(5),
            ),
            Err(VerifyDispatchError::LaneClosed)
        );
        assert!(inspect_handle.requires_session_disconnect());
    }
}
