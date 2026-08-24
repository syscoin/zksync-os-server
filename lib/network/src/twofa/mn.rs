use super::config::MainNode2faConfig;
use super::protocol::{Zks2faPeerHandle, wait_for_lane_cancellation};
use super::wire::Zks2faMessage;
use crate::protocol::ProtocolEvent;
use crate::service::PeerVerifyBatchResult;
use crate::wire::auth::{recover_verifier_signer, validate_canonical_batch_signature};
use crate::wire::verification::{
    MAX_VERIFY_BATCH_REFUSAL_REASON_BYTES, VerifyBatchOutcome, VerifyBatchResult,
};
use alloy::primitives::B256;
use alloy::primitives::bytes::BytesMut;
use futures::{Stream, StreamExt};
use reth_network_peers::PeerId;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

// SYSCOIN: Bound both pre-authentication phases so silent untrusted peers release their capped
// 2FA lane. Ten seconds accommodates normal network scheduling without creating a long-lived slot.
const VERIFIER_AUTH_STEP_TIMEOUT: Duration = Duration::from_secs(10);

// SYSCOIN: Every worker exit closes its shared lane state and wakes registry aliases.
struct CloseLaneOnDrop(Zks2faPeerHandle);

impl Drop for CloseLaneOnDrop {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// Background task that drives the main-node side of a `zks_2fa` connection.
///
/// Authenticates a verifier external node (role request -> challenge -> auth), then forwards any
/// [`VerifyBatchResult`](crate::wire::verification::VerifyBatchResult) the peer returns into the
/// node via `verify_result_tx`. Outbound `VerifyBatch` requests are pushed onto this connection by
/// the verify dispatcher via the connection registry, not from this task.
#[cfg(test)]
pub(super) async fn run_2fa_mn_connection(
    mut conn: impl Stream<Item = Zks2faMessage> + Unpin,
    outbound_tx: mpsc::Sender<BytesMut>,
    events_sender: mpsc::UnboundedSender<ProtocolEvent>,
    peer_id: PeerId,
    config: MainNode2faConfig,
    peer_handle: Zks2faPeerHandle,
) {
    drive_2fa_mn_connection(
        &mut conn,
        outbound_tx,
        events_sender,
        peer_id,
        config,
        peer_handle,
    )
    .await;
}

// SYSCOIN: Production keeps ownership of the typed inbound stream in the protocol supervisor so
// replay-preserving worker exits can release their lane and then actively drain the same stream.
pub(super) async fn drive_2fa_mn_connection(
    conn: &mut (impl Stream<Item = Zks2faMessage> + Unpin),
    outbound_tx: mpsc::Sender<BytesMut>,
    events_sender: mpsc::UnboundedSender<ProtocolEvent>,
    peer_id: PeerId,
    config: MainNode2faConfig,
    peer_handle: Zks2faPeerHandle,
) {
    // SYSCOIN: All exit paths, including timeouts and channel saturation, close the shared lane
    // state before the protocol wrapper releases its registry entry and permit.
    let _close_lane = CloseLaneOnDrop(peer_handle.clone());
    let mut cancellation_rx = peer_handle.cancellation_receiver();
    let mut request_revision_rx = peer_handle.request_revision_receiver();
    if perform_verifier_handshake(
        conn,
        &outbound_tx,
        &events_sender,
        peer_id,
        &config,
        &peer_handle,
        &mut cancellation_rx,
    )
    .await
    .is_err()
    {
        return;
    }
    let verify_result_tx = config.verify_result_tx;

    // SYSCOIN: Race cancellation, the exact outstanding deadline revision, and peer input so a
    // silent or stale verifier cannot retain its lane or satisfy a replacement request.
    loop {
        let message = tokio::select! {
            biased;
            _ = wait_for_lane_cancellation(&mut cancellation_rx) => return,
            generation = wait_for_outstanding_deadline(&peer_handle, &mut request_revision_rx) => {
                if peer_handle.expire_outstanding(generation) {
                    tracing::warn!(%peer_id, generation, "verify request timed out; terminating lane");
                    return;
                }
                continue;
            }
            message = conn.next() => message,
        };
        match message {
            Some(Zks2faMessage::VerifyBatchResult(result)) => {
                // SYSCOIN: Reject noncanonical payloads before consuming the exact outstanding
                // reservation, then use nonblocking admission to the bounded shared channel.
                if !valid_result_payload(&result) {
                    let (outcome, payload_bytes) = result_payload_metadata(&result);
                    tracing::warn!(
                        %peer_id,
                        request_id = result.request_id,
                        batch_number = result.batch_number,
                        outcome,
                        payload_bytes,
                        "invalid verify batch result payload"
                    );
                    peer_handle.close_for_session_recovery();
                    return;
                }
                if let Err(error) =
                    peer_handle.consume_verify_result(result.request_id, result.batch_number)
                {
                    tracing::warn!(%peer_id, ?error, "unexpected verify batch result; terminating");
                    peer_handle.close_for_session_recovery();
                    return;
                }
                match verify_result_tx.try_send(PeerVerifyBatchResult {
                    peer_id,
                    lane_id: peer_handle.lane_id(),
                    message: result,
                }) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        // SYSCOIN: The exact reservation was already consumed, so this result
                        // cannot be replayed safely. Force full-session renegotiation instead of
                        // leaving the optional lane inert and permanently unavailable.
                        peer_handle.close_for_session_recovery();
                        tracing::warn!(%peer_id, "verify result channel is full; restarting peer session");
                        return;
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        tracing::info!(%peer_id, "verify result channel is closed; terminating");
                        return;
                    }
                }
            }
            Some(msg) => {
                // SYSCOIN: Log protocol metadata only; VerifyBatch/Auth/Result payloads may contain
                // large commit data, signatures, or operator diagnostics.
                tracing::info!(
                    message_id = ?msg.message_id(),
                    "received unexpected zks_2fa message from peer; terminating"
                );
                peer_handle.close_for_session_recovery();
                return;
            }
            None => return,
        }
    }
}

// SYSCOIN: Bind the MN handshake, signer recovery, authorization, and emitted events to one lane.
async fn perform_verifier_handshake(
    conn: &mut (impl Stream<Item = Zks2faMessage> + Unpin),
    outbound_tx: &mpsc::Sender<BytesMut>,
    events_sender: &mpsc::UnboundedSender<ProtocolEvent>,
    verifier_peer_id: PeerId,
    config: &MainNode2faConfig,
    peer_handle: &Zks2faPeerHandle,
    cancellation_rx: &mut watch::Receiver<bool>,
) -> Result<(), ()> {
    // SYSCOIN: AwaitingRole is deadline-bound; any other first message is terminal.
    let role_request = tokio::select! {
        biased;
        _ = wait_for_lane_cancellation(cancellation_rx) => return Err(()),
        role_request = tokio::time::timeout(VERIFIER_AUTH_STEP_TIMEOUT, conn.next()) => role_request,
    };
    match role_request {
        Ok(Some(Zks2faMessage::VerifierRoleRequest(_))) => {}
        Ok(Some(other)) => {
            tracing::warn!(%verifier_peer_id, message_id = ?other.message_id(), "expected verifier role request; terminating");
            peer_handle.close_for_session_recovery();
            return Err(());
        }
        Ok(None) => return Err(()),
        Err(_) => {
            // SYSCOIN: The optional capability cannot renegotiate on a live replay session. A
            // transient role timeout therefore restarts only this exact owning RLPx connection.
            peer_handle.close_for_session_recovery();
            tracing::warn!(%verifier_peer_id, "verifier role request timed out; terminating");
            return Err(());
        }
    }
    if !peer_handle.begin_authentication() {
        tracing::warn!(%verifier_peer_id, "verifier lane rejected role transition; terminating");
        return Err(());
    }
    events_sender
        .send(ProtocolEvent::VerifierRoleRequested {
            peer_id: verifier_peer_id,
            lane_id: peer_handle.lane_id(),
        })
        .ok();
    let nonce = B256::random();
    match outbound_tx.try_send(Zks2faMessage::verifier_challenge(nonce).encoded()) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(_)) => {
            // SYSCOIN: A challenge that cannot enter the bounded writer leaves no recoverable
            // handshake state; restart RLPx rather than parking the verifier lane indefinitely.
            peer_handle.close_for_session_recovery();
            tracing::warn!(%verifier_peer_id, channel_state = "full", "failed to enqueue verifier challenge");
            return Err(());
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            tracing::warn!(%verifier_peer_id, channel_state = "closed", "failed to enqueue verifier challenge");
            return Err(());
        }
    }
    events_sender
        .send(ProtocolEvent::VerifierChallengeSent {
            peer_id: verifier_peer_id,
            lane_id: peer_handle.lane_id(),
            nonce,
        })
        .ok();

    // SYSCOIN: AwaitingAuth receives a fresh deadline after the challenge is enqueued.
    let auth = tokio::select! {
        biased;
        _ = wait_for_lane_cancellation(cancellation_rx) => return Err(()),
        auth = tokio::time::timeout(VERIFIER_AUTH_STEP_TIMEOUT, conn.next()) => auth,
    };
    let auth = match auth {
        Ok(Some(Zks2faMessage::VerifierAuth(auth))) => auth,
        Ok(Some(other)) => {
            tracing::warn!(%verifier_peer_id, message_id = ?other.message_id(), "expected verifier auth; terminating");
            peer_handle.close_for_session_recovery();
            return Err(());
        }
        Ok(None) => return Err(()),
        Err(_) => {
            // SYSCOIN: The nonce transcript is single-use and negotiation is connection-scoped;
            // restart the owning RLPx session after a transient authentication timeout.
            peer_handle.close_for_session_recovery();
            tracing::warn!(%verifier_peer_id, "verifier auth timed out; terminating");
            return Err(());
        }
    };
    match recover_verifier_signer(
        config.chain_id,
        config.local_peer_id,
        verifier_peer_id,
        nonce,
        auth.signature.as_ref(),
    ) {
        Ok(signer) if config.accepted_verifier_signers.contains(&signer) => {
            if !peer_handle.authorize() {
                tracing::warn!(%verifier_peer_id, "verifier lane rejected auth transition");
                return Err(());
            }
            events_sender
                .send(ProtocolEvent::VerifierAuthorized {
                    peer_id: verifier_peer_id,
                    lane_id: peer_handle.lane_id(),
                    signer,
                })
                .ok();
            Ok(())
        }
        Ok(signer) => {
            tracing::warn!(%verifier_peer_id, %signer, "peer failed verifier authorization");
            events_sender
                .send(ProtocolEvent::VerifierUnauthorized {
                    peer_id: verifier_peer_id,
                    lane_id: peer_handle.lane_id(),
                    signer: Some(signer),
                })
                .ok();
            Err(())
        }
        Err(error) => {
            tracing::warn!(%verifier_peer_id, %error, "failed to recover verifier signer");
            events_sender
                .send(ProtocolEvent::VerifierUnauthorized {
                    peer_id: verifier_peer_id,
                    lane_id: peer_handle.lane_id(),
                    signer: None,
                })
                .ok();
            Err(())
        }
    }
}

// SYSCOIN: The worker follows reservation revisions and arms only the current exact generation's
// deadline. Revision wakeups replace stale sleeps after a result, retry, close, or replacement.
async fn wait_for_outstanding_deadline(
    peer_handle: &Zks2faPeerHandle,
    request_revision_rx: &mut watch::Receiver<u64>,
) -> u64 {
    loop {
        let _ = *request_revision_rx.borrow_and_update();
        if let Some((generation, deadline)) = peer_handle.outstanding_deadline() {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => return generation,
                changed = request_revision_rx.changed() => {
                    if changed.is_err() {
                        futures::future::pending::<()>().await;
                    }
                }
            }
        } else if request_revision_rx.changed().await.is_err() {
            futures::future::pending::<()>().await;
        }
    }
}

// SYSCOIN: Enforce canonical bounded result payloads before shared-channel admission.
fn valid_result_payload(result: &VerifyBatchResult) -> bool {
    match &result.result {
        VerifyBatchOutcome::Approved(signature) => {
            validate_canonical_batch_signature(signature).is_ok()
        }
        VerifyBatchOutcome::Refused(reason) => {
            reason.len() <= MAX_VERIFY_BATCH_REFUSAL_REASON_BYTES
        }
    }
}

// SYSCOIN: Return only bounded, non-secret result metadata for diagnostics.
fn result_payload_metadata(result: &VerifyBatchResult) -> (&'static str, usize) {
    match &result.result {
        VerifyBatchOutcome::Approved(signature) => ("approved", signature.len()),
        VerifyBatchOutcome::Refused(reason) => ("refused", reason.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::auth::{VerifierAuth, verifier_auth_prehash};
    use crate::wire::verification::{VerifyBatchOutcome, VerifyBatchResult};
    use alloy::primitives::{Bytes, Signature, U256, uint};
    use alloy::signers::{SignerSync, local::PrivateKeySigner};
    use futures::{channel::mpsc as futures_mpsc, stream};
    use std::str::FromStr;
    use std::time::Duration;

    const TEST_SIGNING_KEY: &str =
        "0x7726827caac94a7f9e1b160f7ea819f172f7b6f9d2a97f992c38edeab82d4110";
    const TEST_CHAIN_ID: u64 = 57_057;
    const TEST_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

    fn test_peer_id() -> PeerId {
        PeerId::repeat_byte(0x11)
    }

    fn test_main_node_peer_id() -> PeerId {
        PeerId::repeat_byte(0x22)
    }

    fn test_result() -> VerifyBatchResult {
        VerifyBatchResult {
            request_id: 41,
            batch_number: 7,
            result: VerifyBatchOutcome::Refused("test refusal".to_owned()),
        }
    }

    fn test_signer() -> PrivateKeySigner {
        PrivateKeySigner::from_str(TEST_SIGNING_KEY).unwrap()
    }

    fn decode_challenge(encoded: BytesMut) -> B256 {
        let mut encoded = encoded.as_ref();
        let message = Zks2faMessage::decode_message(&mut encoded).unwrap();
        assert!(encoded.is_empty());
        let Zks2faMessage::VerifierChallenge(challenge) = message else {
            panic!("expected verifier challenge");
        };
        challenge.nonce
    }

    async fn send_valid_auth(
        inbound_tx: &futures_mpsc::UnboundedSender<Zks2faMessage>,
        outbound_rx: &mut mpsc::Receiver<BytesMut>,
        signer: &PrivateKeySigner,
    ) {
        inbound_tx
            .unbounded_send(Zks2faMessage::verifier_role_request())
            .unwrap();
        let encoded = tokio::time::timeout(Duration::from_secs(1), outbound_rx.recv())
            .await
            .expect("challenge send timed out")
            .expect("connection ended before challenge");
        let nonce = decode_challenge(encoded);
        let signature = signer
            .sign_hash_sync(&verifier_auth_prehash(
                TEST_CHAIN_ID,
                test_main_node_peer_id(),
                test_peer_id(),
                nonce,
            ))
            .unwrap();
        inbound_tx
            .unbounded_send(Zks2faMessage::VerifierAuth(VerifierAuth {
                signature: signature.as_bytes().to_vec().into(),
            }))
            .unwrap();
    }

    async fn wait_for_authorized(events_rx: &mut mpsc::UnboundedReceiver<ProtocolEvent>) {
        loop {
            match tokio::time::timeout(Duration::from_secs(1), events_rx.recv())
                .await
                .expect("authorization event timed out")
                .expect("event channel closed before authorization")
            {
                ProtocolEvent::VerifierAuthorized { .. } => return,
                ProtocolEvent::VerifierUnauthorized { signer, .. } => {
                    panic!("unexpected authorization failure: {signer:?}")
                }
                _ => {}
            }
        }
    }

    fn reserve_result(
        peer_handle: &Zks2faPeerHandle,
        outbound_rx: &mut mpsc::Receiver<BytesMut>,
        request_id: u64,
        batch_number: u64,
        request_timeout: Duration,
    ) {
        peer_handle
            .try_enqueue_verify_batch(
                request_id,
                batch_number,
                BytesMut::from(&b"reserved request"[..]),
                tokio::time::Instant::now() + request_timeout,
            )
            .unwrap();
        assert_eq!(outbound_rx.try_recv().unwrap(), &b"reserved request"[..]);
    }

    struct TestConnection {
        inbound_tx: futures_mpsc::UnboundedSender<Zks2faMessage>,
        outbound_rx: mpsc::Receiver<BytesMut>,
        events_rx: mpsc::UnboundedReceiver<ProtocolEvent>,
        verify_result_tx: mpsc::Sender<PeerVerifyBatchResult>,
        verify_result_rx: mpsc::Receiver<PeerVerifyBatchResult>,
        peer_handle: Zks2faPeerHandle,
        request_timeout: Duration,
        task: tokio::task::JoinHandle<()>,
    }

    impl TestConnection {
        fn spawn(
            accepted_verifier_signers: Vec<alloy::primitives::Address>,
            capacity: usize,
        ) -> Self {
            Self::spawn_with_timeout(accepted_verifier_signers, capacity, TEST_REQUEST_TIMEOUT)
        }

        fn spawn_with_timeout(
            accepted_verifier_signers: Vec<alloy::primitives::Address>,
            capacity: usize,
            request_timeout: Duration,
        ) -> Self {
            let (inbound_tx, inbound_rx) = futures_mpsc::unbounded();
            let (outbound_tx, outbound_rx) = mpsc::channel(8);
            let (events_sender, events_rx) = mpsc::unbounded_channel();
            let (verify_result_tx, verify_result_rx) = mpsc::channel(capacity);
            let peer_handle = Zks2faPeerHandle::new(outbound_tx.clone());
            let task = tokio::spawn(run_2fa_mn_connection(
                inbound_rx,
                outbound_tx,
                events_sender,
                test_peer_id(),
                MainNode2faConfig {
                    chain_id: TEST_CHAIN_ID,
                    local_peer_id: test_main_node_peer_id(),
                    accepted_verifier_signers,
                    verify_result_tx: verify_result_tx.clone(),
                },
                peer_handle.clone(),
            ));
            Self {
                inbound_tx,
                outbound_rx,
                events_rx,
                verify_result_tx,
                verify_result_rx,
                peer_handle,
                request_timeout,
                task,
            }
        }

        async fn authenticate(&mut self, signer: &PrivateKeySigner) {
            send_valid_auth(&self.inbound_tx, &mut self.outbound_rx, signer).await;
            wait_for_authorized(&mut self.events_rx).await;
        }

        fn reserve(&mut self, request_id: u64, batch_number: u64) {
            reserve_result(
                &self.peer_handle,
                &mut self.outbound_rx,
                request_id,
                batch_number,
                self.request_timeout,
            );
        }

        async fn wait_terminated(&mut self) {
            tokio::time::timeout(Duration::from_secs(1), &mut self.task)
                .await
                .expect("connection task did not terminate")
                .unwrap();
        }
    }

    async fn run_fixed_messages(
        messages: Vec<Zks2faMessage>,
        accepted_verifier_signers: Vec<alloy::primitives::Address>,
    ) -> (
        mpsc::Receiver<BytesMut>,
        mpsc::UnboundedReceiver<ProtocolEvent>,
        mpsc::Receiver<PeerVerifyBatchResult>,
        Zks2faPeerHandle,
    ) {
        let (outbound_tx, outbound_rx) = mpsc::channel(8);
        let (events_sender, events_rx) = mpsc::unbounded_channel();
        let (verify_result_tx, verify_result_rx) = mpsc::channel(1);
        let peer_handle = Zks2faPeerHandle::new(outbound_tx.clone());
        let inspect_handle = peer_handle.clone();
        run_2fa_mn_connection(
            stream::iter(messages),
            outbound_tx,
            events_sender,
            test_peer_id(),
            MainNode2faConfig {
                chain_id: TEST_CHAIN_ID,
                local_peer_id: test_main_node_peer_id(),
                accepted_verifier_signers,
                verify_result_tx,
            },
            peer_handle,
        )
        .await;
        (outbound_rx, events_rx, verify_result_rx, inspect_handle)
    }

    #[tokio::test]
    async fn unauthenticated_result_never_enters_shared_channel() {
        let (_outbound_rx, _events_rx, mut verify_result_rx, peer_handle) = run_fixed_messages(
            vec![Zks2faMessage::VerifyBatchResult(test_result())],
            vec![],
        )
        .await;

        assert!(
            verify_result_rx.try_recv().is_err(),
            "pre-authentication result must be rejected before the bounded channel"
        );
        assert!(peer_handle.requires_session_disconnect());
    }

    #[tokio::test]
    async fn repeated_role_request_terminates_without_rotating_challenge() {
        let (mut outbound_rx, mut events_rx, mut verify_result_rx, peer_handle) =
            run_fixed_messages(
                vec![
                    Zks2faMessage::verifier_role_request(),
                    Zks2faMessage::verifier_role_request(),
                    Zks2faMessage::VerifyBatchResult(test_result()),
                ],
                vec![],
            )
            .await;

        decode_challenge(outbound_rx.try_recv().expect("first challenge is sent"));
        assert!(
            outbound_rx.try_recv().is_err(),
            "repeated role request must not replace the pending challenge"
        );
        assert!(matches!(
            events_rx.try_recv(),
            Ok(ProtocolEvent::VerifierRoleRequested { .. })
        ));
        assert!(matches!(
            events_rx.try_recv(),
            Ok(ProtocolEvent::VerifierChallengeSent { .. })
        ));
        assert!(
            events_rx.try_recv().is_err(),
            "repeated negotiation must terminate before emitting another role event"
        );
        assert!(verify_result_rx.try_recv().is_err());
        assert!(peer_handle.requires_session_disconnect());
    }

    #[tokio::test]
    async fn failed_auth_terminates_before_followup_result() {
        let (mut outbound_rx, mut events_rx, mut verify_result_rx, peer_handle) =
            run_fixed_messages(
                vec![
                    Zks2faMessage::verifier_role_request(),
                    Zks2faMessage::VerifierAuth(VerifierAuth {
                        signature: Bytes::from(vec![7_u8; 64]),
                    }),
                    Zks2faMessage::VerifyBatchResult(test_result()),
                ],
                vec![],
            )
            .await;

        decode_challenge(outbound_rx.try_recv().expect("challenge is sent"));
        assert!(matches!(
            events_rx.try_recv(),
            Ok(ProtocolEvent::VerifierRoleRequested { .. })
        ));
        assert!(matches!(
            events_rx.try_recv(),
            Ok(ProtocolEvent::VerifierChallengeSent { .. })
        ));
        assert!(matches!(
            events_rx.try_recv(),
            Ok(ProtocolEvent::VerifierUnauthorized { signer: None, .. })
        ));
        assert!(events_rx.try_recv().is_err());
        assert!(
            verify_result_rx.try_recv().is_err(),
            "failed authentication must terminate before processing later frames"
        );
        assert!(!peer_handle.requires_session_disconnect());
    }

    #[tokio::test]
    async fn authorized_result_is_forwarded() {
        let signer = test_signer();
        let (inbound_tx, inbound_rx) = futures_mpsc::unbounded();
        let (outbound_tx, mut outbound_rx) = mpsc::channel(8);
        let (events_sender, mut events_rx) = mpsc::unbounded_channel();
        let (verify_result_tx, mut verify_result_rx) = mpsc::channel(1);
        let peer_handle = Zks2faPeerHandle::new(outbound_tx.clone());
        let task = tokio::spawn(run_2fa_mn_connection(
            inbound_rx,
            outbound_tx,
            events_sender,
            test_peer_id(),
            MainNode2faConfig {
                chain_id: TEST_CHAIN_ID,
                local_peer_id: test_main_node_peer_id(),
                accepted_verifier_signers: vec![signer.address()],
                verify_result_tx,
            },
            peer_handle.clone(),
        ));

        send_valid_auth(&inbound_tx, &mut outbound_rx, &signer).await;
        wait_for_authorized(&mut events_rx).await;
        reserve_result(&peer_handle, &mut outbound_rx, 41, 7, TEST_REQUEST_TIMEOUT);
        let expected = test_result();
        inbound_tx
            .unbounded_send(Zks2faMessage::VerifyBatchResult(expected.clone()))
            .unwrap();

        let forwarded = tokio::time::timeout(Duration::from_secs(1), verify_result_rx.recv())
            .await
            .expect("result forwarding timed out")
            .expect("result channel closed");
        assert_eq!(forwarded.peer_id, test_peer_id());
        assert_eq!(forwarded.lane_id, peer_handle.lane_id());
        assert_eq!(forwarded.message, expected);
        drop(inbound_tx);
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("connection task did not terminate")
            .unwrap();
    }

    #[tokio::test]
    async fn valid_but_unaccepted_signer_is_terminal() {
        let signer = test_signer();
        let (inbound_tx, inbound_rx) = futures_mpsc::unbounded();
        let (outbound_tx, mut outbound_rx) = mpsc::channel(8);
        let (events_sender, mut events_rx) = mpsc::unbounded_channel();
        let (verify_result_tx, mut verify_result_rx) = mpsc::channel(1);
        let peer_handle = Zks2faPeerHandle::new(outbound_tx.clone());
        let inspect_handle = peer_handle.clone();
        let task = tokio::spawn(run_2fa_mn_connection(
            inbound_rx,
            outbound_tx,
            events_sender,
            test_peer_id(),
            MainNode2faConfig {
                chain_id: TEST_CHAIN_ID,
                local_peer_id: test_main_node_peer_id(),
                accepted_verifier_signers: vec![],
                verify_result_tx,
            },
            peer_handle,
        ));

        send_valid_auth(&inbound_tx, &mut outbound_rx, &signer).await;
        inbound_tx
            .unbounded_send(Zks2faMessage::VerifyBatchResult(test_result()))
            .unwrap();
        drop(inbound_tx);

        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("connection task did not terminate after rejected signer")
            .unwrap();
        assert!(
            verify_result_rx.try_recv().is_err(),
            "rejected signer must not process any later result"
        );
        assert!(matches!(
            events_rx.try_recv(),
            Ok(ProtocolEvent::VerifierRoleRequested { .. })
        ));
        assert!(matches!(
            events_rx.try_recv(),
            Ok(ProtocolEvent::VerifierChallengeSent { .. })
        ));
        assert!(matches!(
            events_rx.try_recv(),
            Ok(ProtocolEvent::VerifierUnauthorized {
                signer: Some(recovered),
                ..
            }) if recovered == signer.address()
        ));
        assert!(events_rx.try_recv().is_err());
        assert!(!inspect_handle.requires_session_disconnect());
    }

    #[tokio::test]
    async fn authorized_peer_cannot_renegotiate_role() {
        let signer = test_signer();
        let (inbound_tx, inbound_rx) = futures_mpsc::unbounded();
        let (outbound_tx, mut outbound_rx) = mpsc::channel(8);
        let (events_sender, mut events_rx) = mpsc::unbounded_channel();
        let (verify_result_tx, mut verify_result_rx) = mpsc::channel(1);
        let peer_handle = Zks2faPeerHandle::new(outbound_tx.clone());
        let inspect_handle = peer_handle.clone();
        let task = tokio::spawn(run_2fa_mn_connection(
            inbound_rx,
            outbound_tx,
            events_sender,
            test_peer_id(),
            MainNode2faConfig {
                chain_id: TEST_CHAIN_ID,
                local_peer_id: test_main_node_peer_id(),
                accepted_verifier_signers: vec![signer.address()],
                verify_result_tx,
            },
            peer_handle,
        ));

        send_valid_auth(&inbound_tx, &mut outbound_rx, &signer).await;
        inbound_tx
            .unbounded_send(Zks2faMessage::verifier_role_request())
            .unwrap();
        inbound_tx
            .unbounded_send(Zks2faMessage::VerifyBatchResult(test_result()))
            .unwrap();
        drop(inbound_tx);

        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("connection task did not terminate on repeated role request")
            .unwrap();
        assert!(
            verify_result_rx.try_recv().is_err(),
            "result after repeated role negotiation must not be processed"
        );
        assert!(matches!(
            events_rx.try_recv(),
            Ok(ProtocolEvent::VerifierRoleRequested { .. })
        ));
        assert!(matches!(
            events_rx.try_recv(),
            Ok(ProtocolEvent::VerifierChallengeSent { .. })
        ));
        assert!(matches!(
            events_rx.try_recv(),
            Ok(ProtocolEvent::VerifierAuthorized { .. })
        ));
        assert!(
            events_rx.try_recv().is_err(),
            "renegotiation must not emit another role-request event"
        );
        assert!(
            outbound_rx.try_recv().is_err(),
            "renegotiation must not send another challenge"
        );
        assert!(inspect_handle.requires_session_disconnect());
    }

    #[test]
    fn result_payload_bounds_are_canonical() {
        let result = |outcome| VerifyBatchResult {
            request_id: 41,
            batch_number: 7,
            result: outcome,
        };
        let canonical = test_signer()
            .sign_hash_sync(&B256::repeat_byte(0xA5))
            .unwrap()
            .as_bytes();
        assert!(valid_result_payload(&result(VerifyBatchOutcome::Approved(
            Bytes::copy_from_slice(&canonical),
        ))));
        assert!(!valid_result_payload(&result(
            VerifyBatchOutcome::Approved(Bytes::from(vec![0_u8; 64]),)
        )));
        let mut bad_parity = canonical;
        bad_parity[64] = 0;
        assert!(!valid_result_payload(&result(
            VerifyBatchOutcome::Approved(Bytes::copy_from_slice(&bad_parity))
        )));
        const SECP256K1_ORDER: U256 =
            uint!(0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141_U256);
        let parsed = Signature::from_raw_array(&canonical).unwrap();
        let mut high_s = canonical;
        high_s[32..64].copy_from_slice(&(SECP256K1_ORDER - parsed.s()).to_be_bytes::<32>());
        high_s[64] = if canonical[64] == 27 { 28 } else { 27 };
        assert!(!valid_result_payload(&result(
            VerifyBatchOutcome::Approved(Bytes::copy_from_slice(&high_s))
        )));
        assert!(valid_result_payload(&result(VerifyBatchOutcome::Refused(
            "r".repeat(MAX_VERIFY_BATCH_REFUSAL_REASON_BYTES),
        ))));
        assert!(!valid_result_payload(&result(VerifyBatchOutcome::Refused(
            "r".repeat(MAX_VERIFY_BATCH_REFUSAL_REASON_BYTES + 1),
        ))));
    }

    #[tokio::test]
    async fn oversized_result_is_terminal_before_shared_channel() {
        let signer = test_signer();
        let mut connection = TestConnection::spawn(vec![signer.address()], 1);
        connection.authenticate(&signer).await;
        connection.reserve(41, 7);
        connection
            .inbound_tx
            .unbounded_send(Zks2faMessage::VerifyBatchResult(VerifyBatchResult {
                request_id: 41,
                batch_number: 7,
                result: VerifyBatchOutcome::Approved(Bytes::from(vec![0_u8; 66])),
            }))
            .unwrap();

        connection.wait_terminated().await;
        assert!(connection.verify_result_rx.try_recv().is_err());
        assert!(connection.peer_handle.requires_session_disconnect());
    }

    #[tokio::test]
    async fn mismatched_result_is_terminal_and_not_forwarded() {
        let signer = test_signer();
        let mut connection = TestConnection::spawn(vec![signer.address()], 1);
        connection.authenticate(&signer).await;
        connection.reserve(41, 7);
        let mut mismatched = test_result();
        mismatched.request_id = 42;
        connection
            .inbound_tx
            .unbounded_send(Zks2faMessage::VerifyBatchResult(mismatched))
            .unwrap();

        connection.wait_terminated().await;
        assert!(connection.verify_result_rx.try_recv().is_err());
        assert!(connection.peer_handle.requires_session_disconnect());
    }

    #[tokio::test]
    async fn duplicate_result_is_forwarded_once_then_terminates_lane() {
        let signer = test_signer();
        let mut connection = TestConnection::spawn(vec![signer.address()], 2);
        connection.authenticate(&signer).await;
        connection.reserve(41, 7);
        let result = test_result();
        connection
            .inbound_tx
            .unbounded_send(Zks2faMessage::VerifyBatchResult(result.clone()))
            .unwrap();
        connection
            .inbound_tx
            .unbounded_send(Zks2faMessage::VerifyBatchResult(result.clone()))
            .unwrap();

        connection.wait_terminated().await;
        assert_eq!(
            connection.verify_result_rx.try_recv().unwrap().message,
            result
        );
        assert!(connection.verify_result_rx.try_recv().is_err());
        assert!(connection.peer_handle.requires_session_disconnect());
    }

    #[tokio::test]
    async fn full_shared_result_channel_marks_lane_for_session_recovery() {
        let signer = test_signer();
        let mut connection = TestConnection::spawn(vec![signer.address()], 1);
        connection.authenticate(&signer).await;
        connection.reserve(41, 7);
        let occupying = PeerVerifyBatchResult {
            peer_id: PeerId::repeat_byte(0x99),
            lane_id: 99,
            message: test_result(),
        };
        connection
            .verify_result_tx
            .try_send(occupying.clone())
            .unwrap();
        connection
            .inbound_tx
            .unbounded_send(Zks2faMessage::VerifyBatchResult(test_result()))
            .unwrap();

        connection.wait_terminated().await;
        let retained = connection.verify_result_rx.try_recv().unwrap();
        assert_eq!(retained.peer_id, occupying.peer_id);
        assert_eq!(retained.message, occupying.message);
        assert!(connection.verify_result_rx.try_recv().is_err());
        assert_eq!(
            connection.peer_handle.try_enqueue_verify_batch(
                42,
                8,
                BytesMut::from(&b"after saturated result"[..]),
                tokio::time::Instant::now() + TEST_REQUEST_TIMEOUT,
            ),
            Err(crate::twofa::protocol::VerifyDispatchError::LaneClosed),
        );
        assert!(connection.peer_handle.requires_session_disconnect());
    }

    #[tokio::test(start_paused = true)]
    async fn outstanding_request_expiry_closes_and_wakes_worker() {
        use crate::twofa::protocol::VerifyDispatchError;

        let signer = test_signer();
        let request_timeout = Duration::from_secs(5);
        let mut connection =
            TestConnection::spawn_with_timeout(vec![signer.address()], 1, request_timeout);
        connection.authenticate(&signer).await;
        connection.reserve(41, 7);

        tokio::time::advance(request_timeout).await;
        connection.wait_terminated().await;

        assert_eq!(
            connection.peer_handle.try_enqueue_verify_batch(
                42,
                8,
                BytesMut::from(&b"after timeout"[..]),
                tokio::time::Instant::now() + TEST_REQUEST_TIMEOUT,
            ),
            Err(VerifyDispatchError::LaneClosed)
        );
        assert!(connection.verify_result_rx.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn result_ready_at_expiry_is_late_and_never_forwarded() {
        let signer = test_signer();
        let request_timeout = Duration::from_secs(5);
        let mut connection =
            TestConnection::spawn_with_timeout(vec![signer.address()], 1, request_timeout);
        connection.authenticate(&signer).await;
        connection.reserve(41, 7);

        tokio::time::advance(request_timeout).await;
        connection
            .inbound_tx
            .unbounded_send(Zks2faMessage::VerifyBatchResult(test_result()))
            .unwrap();
        connection.wait_terminated().await;

        assert!(connection.verify_result_rx.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn stale_deadline_cannot_clear_newer_retry_reservation() {
        let request_timeout = Duration::from_secs(5);
        let (outbound_tx, mut outbound_rx) = mpsc::channel(2);
        let handle = Zks2faPeerHandle::new(outbound_tx);
        assert!(handle.begin_authentication());
        assert!(handle.authorize());
        handle
            .try_enqueue_verify_batch(
                41,
                7,
                BytesMut::from(&b"first"[..]),
                tokio::time::Instant::now() + request_timeout,
            )
            .unwrap();
        outbound_rx.try_recv().unwrap();
        let (old_generation, _) = handle.outstanding_deadline().unwrap();

        tokio::time::advance(Duration::from_secs(2)).await;
        handle.consume_verify_result(41, 7).unwrap();
        handle
            .try_enqueue_verify_batch(
                41,
                7,
                BytesMut::from(&b"retry"[..]),
                tokio::time::Instant::now() + request_timeout,
            )
            .unwrap();
        outbound_rx.try_recv().unwrap();
        let (new_generation, _) = handle.outstanding_deadline().unwrap();
        assert_ne!(old_generation, new_generation);

        tokio::time::advance(Duration::from_secs(3)).await;
        assert!(!handle.expire_outstanding(old_generation));
        assert!(handle.is_authorized());
        handle.consume_verify_result(41, 7).unwrap();
    }

    // SYSCOIN: A full local writer is transient but unrecoverable on the current capability
    // transcript, while a closed writer is local teardown and must not create reconnect churn.
    #[tokio::test]
    async fn challenge_writer_full_recovers_but_closed_writer_preserves_session_policy() {
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
            let (events_sender, _events_rx) = mpsc::unbounded_channel();
            let (verify_result_tx, _verify_result_rx) = mpsc::channel(1);
            let config = MainNode2faConfig {
                chain_id: TEST_CHAIN_ID,
                local_peer_id: test_main_node_peer_id(),
                accepted_verifier_signers: vec![test_signer().address()],
                verify_result_tx,
            };
            let handle = Zks2faPeerHandle::new(outbound_tx.clone());
            let mut cancellation_rx = handle.cancellation_receiver();
            let mut conn = stream::iter([Zks2faMessage::verifier_role_request()]);
            assert!(
                perform_verifier_handshake(
                    &mut conn,
                    &outbound_tx,
                    &events_sender,
                    test_peer_id(),
                    &config,
                    &handle,
                    &mut cancellation_rx,
                )
                .await
                .is_err()
            );
            // Keep the receiver alive through the call in the full case.
            if let Some(outbound_rx) = outbound_rx.as_mut() {
                let _ = outbound_rx.try_recv();
            }
            handle
        }

        assert!(run(false).await.requires_session_disconnect());
        assert!(!run(true).await.requires_session_disconnect());
    }

    #[tokio::test(start_paused = true)]
    async fn silent_peer_cannot_hold_awaiting_role_slot() {
        use crate::twofa::protocol::VerifyDispatchError;

        let signer = test_signer();
        let mut connection = TestConnection::spawn(vec![signer.address()], 1);
        tokio::task::yield_now().await;
        tokio::time::advance(VERIFIER_AUTH_STEP_TIMEOUT).await;
        connection.wait_terminated().await;

        assert_eq!(
            connection.peer_handle.try_enqueue_verify_batch(
                41,
                7,
                BytesMut::from(&b"request"[..]),
                tokio::time::Instant::now() + TEST_REQUEST_TIMEOUT,
            ),
            Err(VerifyDispatchError::LaneClosed)
        );
        assert!(connection.peer_handle.requires_session_disconnect());
    }

    #[tokio::test(start_paused = true)]
    async fn silent_peer_cannot_hold_awaiting_auth_slot() {
        use crate::twofa::protocol::VerifyDispatchError;

        let signer = test_signer();
        let mut connection = TestConnection::spawn(vec![signer.address()], 1);
        connection
            .inbound_tx
            .unbounded_send(Zks2faMessage::verifier_role_request())
            .unwrap();
        let challenge = connection.outbound_rx.recv().await.unwrap();
        decode_challenge(challenge);
        tokio::time::advance(VERIFIER_AUTH_STEP_TIMEOUT).await;
        connection.wait_terminated().await;

        assert_eq!(
            connection.peer_handle.try_enqueue_verify_batch(
                41,
                7,
                BytesMut::from(&b"request"[..]),
                tokio::time::Instant::now() + TEST_REQUEST_TIMEOUT,
            ),
            Err(VerifyDispatchError::LaneClosed)
        );
        assert!(connection.peer_handle.requires_session_disconnect());
    }
}
