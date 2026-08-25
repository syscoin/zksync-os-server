use alloy::primitives::{Address, B256, BlockNumber, Bytes, U256};
use alloy::signers::SignerSync;
use alloy::signers::local::PrivateKeySigner;
use assert_matches::assert_matches;
use futures::StreamExt;
use reth_network::events::PeerEvent;
use reth_network::test_utils::Peer;
use reth_network::{NetworkEvent, Peers, PeersInfo, test_utils::Testnet};
use reth_network_peers::PeerId;
use reth_provider::test_utils::MockEthProvider;
use reth_provider::{BlockReader, HeaderProvider};
use secrecy::{ExposeSecret, SecretString};
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use tokio::sync::{broadcast, mpsc};
use zksync_os_metadata::NODE_SEMVER_VERSION;
use zksync_os_network::protocol::{
    ExternalNodeProtocolConfig, HandlerSharedState, ProtocolEvent, SessionActivationRegistry,
    ZksProtocolHandler,
};
use zksync_os_network::twofa::{
    ExternalNode2faConfig, MainNode2faConfig, Zks2faConnectionRegistry, Zks2faProtocolHandler,
};
use zksync_os_network::version::{ZksProtocolV0, ZksProtocolV5, ZksProtocolVersionSpec};
use zksync_os_network::{
    PeerVerifyBatch, PeerVerifyBatchResult, RecordOverride, VerifyBatch, VerifyBatchOutcome,
    VerifyBatchResult,
};
use zksync_os_storage_api::BlockContext;
use zksync_os_storage_api::{ReadReplay, ReplayRecord};
use zksync_os_types::{BlockStartCursors, NodeRole, ProtocolSemanticVersion};

#[derive(Debug, Clone, Default)]
struct InMemReplay {
    canonical: HashMap<BlockNumber, ReplayRecord>,
    /// Rows stored under explicit db keys, mirroring how reverted records are kept in RocksDB.
    by_key: HashMap<Vec<u8>, ReplayRecord>,
}

impl InMemReplay {
    fn new(replays: impl IntoIterator<Item = (BlockNumber, ReplayRecord)>) -> Self {
        Self {
            canonical: HashMap::from_iter(replays),
            by_key: HashMap::new(),
        }
    }

    fn with_override(mut self, db_key: Vec<u8>, record: ReplayRecord) -> Self {
        self.by_key.insert(db_key, record);
        self
    }
}

impl ReadReplay for InMemReplay {
    fn get_context(&self, block_number: BlockNumber) -> Option<BlockContext> {
        self.canonical.get(&block_number).map(|r| r.block_context)
    }

    fn get_replay_record_by_key(
        &self,
        block_number: BlockNumber,
        db_key: Option<Vec<u8>>,
    ) -> Option<ReplayRecord> {
        match db_key {
            Some(db_key) => self.by_key.get(&db_key).cloned(),
            None => self.canonical.get(&block_number).cloned(),
        }
    }

    fn get_canonical_block_hash(&self, block_number: BlockNumber) -> Option<B256> {
        self.canonical
            .contains_key(&block_number)
            .then(|| B256::from(U256::from(block_number)))
    }

    fn latest_record(&self) -> BlockNumber {
        self.canonical.keys().last().copied().unwrap_or_default()
    }
}

fn dummy_record<P: ZksProtocolVersionSpec>(block_number: BlockNumber) -> ReplayRecord {
    // Do full round conversion ReplayRecord->P::Record->ReplayRecord to get rid of unsupported
    // fields for each protocol version (e.g. everything except the block number is dropped for
    // the test-only v0).
    let record = ReplayRecord::new(
        BlockContext {
            block_number,
            ..Default::default()
        },
        vec![],
        24,
        // Important that this is set to `NODE_SEMVER_VERSION` as the wire formats do not transport
        // node version over the network. Instead, receiver stamps all records with its current
        // node version.
        NODE_SEMVER_VERSION.clone(),
        ProtocolSemanticVersion::new(4, 5, 6),
        B256::random(),
        vec![],
        BlockStartCursors {
            l1_priority_id: 42,
            interop_root_id: 0,
            migration_number: 123,
            interop_fee_number: 456,
        },
    );
    let zks_record: P::Record = record.into();
    zks_record
        .try_into()
        .expect("failed to do full round conversion")
}

fn default_verifier_signing_key() -> SecretString {
    SecretString::from("0x7726827caac94a7f9e1b160f7ea819f172f7b6f9d2a97f992c38edeab82d4110")
}

fn alternate_verifier_signing_key() -> SecretString {
    SecretString::from("0x59c6995e998f97a5a0044966f094538e5f7d918e2f8b3bf3f1e9465d9b38787e")
}

const TEST_CHAIN_ID: u64 = 57_057;
const TEST_VERIFY_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

fn accepted_verifier_signers() -> Vec<Address> {
    vec![
        PrivateKeySigner::from_str(default_verifier_signing_key().expose_secret())
            .unwrap()
            .address(),
    ]
}

fn dummy_verify_batch_result() -> VerifyBatchResult {
    // SYSCOIN: Network admission enforces the same canonical recoverable encoding as the batch
    // collector; use a real low-s signature even though this transport test does not recover it.
    let signature = PrivateKeySigner::from_str(default_verifier_signing_key().expose_secret())
        .unwrap()
        .sign_hash_sync(&B256::repeat_byte(0xA5))
        .unwrap()
        .as_bytes();
    VerifyBatchResult {
        request_id: 41,
        batch_number: 7,
        result: VerifyBatchOutcome::Approved(Bytes::copy_from_slice(&signature)),
    }
}

fn dummy_verify_batch() -> VerifyBatch {
    VerifyBatch {
        request_id: 41,
        batch_number: 7,
        first_block_number: 1,
        last_block_number: 1,
        pubdata_mode: 0,
        commit_data: Bytes::from_static(b"commit"),
        prev_commit_data: Bytes::from_static(b"prev"),
        execution_protocol_version: 32,
    }
}

struct TestPeerProtocolHandles {
    protocol_rx: mpsc::UnboundedReceiver<ProtocolEvent>,
    replay_rx: mpsc::Receiver<ReplayRecord>,
    verify_result_rx: Option<mpsc::Receiver<PeerVerifyBatchResult>>,
    verify_batch_rx: Option<mpsc::Receiver<PeerVerifyBatch>>,
    outgoing_verify_results_tx: Option<broadcast::Sender<PeerVerifyBatchResult>>,
    zks_2fa_registry: Option<Zks2faConnectionRegistry>,
}

trait PeerExt {
    /// SYSCOIN: Test peers mirror production by activating tentative handlers only from Reth's
    /// exact post-deduplication accepted-session event.
    fn install_session_activation_bridge(&self, registry: SessionActivationRegistry);

    fn add_zks_sub_protocol<P: ZksProtocolVersionSpec>(
        &mut self,
        node_role: NodeRole,
        starting_block: BlockNumber,
        replays: impl IntoIterator<Item = (BlockNumber, ReplayRecord)>,
        max_active_connections: usize,
        trusted_main_node_peers: HashSet<PeerId>,
    ) -> (
        mpsc::UnboundedReceiver<ProtocolEvent>,
        mpsc::Receiver<ReplayRecord>,
    );

    fn add_zks_sub_protocol_with_test_handles<P: ZksProtocolVersionSpec>(
        &mut self,
        node_role: NodeRole,
        starting_block: BlockNumber,
        replays: impl IntoIterator<Item = (BlockNumber, ReplayRecord)>,
        max_active_connections: usize,
        trusted_peers: HashSet<PeerId>,
    ) -> TestPeerProtocolHandles;

    fn add_zks_sub_protocol_with_storage<P: ZksProtocolVersionSpec>(
        &mut self,
        node_role: NodeRole,
        starting_block: BlockNumber,
        replays: InMemReplay,
        record_overrides: Vec<RecordOverride>,
        max_active_connections: usize,
        trusted_peers: HashSet<PeerId>,
    ) -> TestPeerProtocolHandles;

    /// Registers production `zks/5` replay and `zks_2fa` verification on the same peer.
    ///
    /// Both handlers publish into one `ProtocolEvent` stream, matching production. Only an
    /// external-node peer needs `verifier_signing_key`; trusted peers apply to both handlers.
    fn add_zks_2fa_sub_protocol(
        &mut self,
        node_role: NodeRole,
        starting_block: BlockNumber,
        replays: impl IntoIterator<Item = (BlockNumber, ReplayRecord)>,
        max_active_connections: (usize, usize),
        verifier_signing_key: Option<SecretString>,
        trusted_main_node_peers: HashSet<PeerId>,
    ) -> TestPeerProtocolHandles;
}

impl<C> PeerExt for Peer<C>
where
    C: BlockReader + HeaderProvider + Clone + 'static,
{
    fn install_session_activation_bridge(&self, registry: SessionActivationRegistry) {
        let mut events = self.peer_handle().event_listener();
        tokio::spawn(async move {
            while let Some(event) = events.next().await {
                if let NetworkEvent::ActivePeerSession { info, .. } = event {
                    registry.activate(info.peer_id, info.remote_addr);
                }
            }
        });
    }

    fn add_zks_sub_protocol<P: ZksProtocolVersionSpec>(
        &mut self,
        node_role: NodeRole,
        starting_block: BlockNumber,
        replays: impl IntoIterator<Item = (BlockNumber, ReplayRecord)>,
        max_active_connections: usize,
        trusted_main_node_peers: HashSet<PeerId>,
    ) -> (
        mpsc::UnboundedReceiver<ProtocolEvent>,
        mpsc::Receiver<ReplayRecord>,
    ) {
        let TestPeerProtocolHandles {
            protocol_rx,
            replay_rx,
            ..
        } = self.add_zks_sub_protocol_with_test_handles::<P>(
            node_role,
            starting_block,
            replays,
            max_active_connections,
            trusted_main_node_peers,
        );
        (protocol_rx, replay_rx)
    }

    fn add_zks_sub_protocol_with_test_handles<P: ZksProtocolVersionSpec>(
        &mut self,
        node_role: NodeRole,
        starting_block: BlockNumber,
        replays: impl IntoIterator<Item = (BlockNumber, ReplayRecord)>,
        max_active_connections: usize,
        trusted_peers: HashSet<PeerId>,
    ) -> TestPeerProtocolHandles {
        self.add_zks_sub_protocol_with_storage::<P>(
            node_role,
            starting_block,
            InMemReplay::new(replays),
            vec![],
            max_active_connections,
            trusted_peers,
        )
    }

    fn add_zks_sub_protocol_with_storage<P: ZksProtocolVersionSpec>(
        &mut self,
        node_role: NodeRole,
        starting_block: BlockNumber,
        replays: InMemReplay,
        record_overrides: Vec<RecordOverride>,
        max_active_connections: usize,
        trusted_peers: HashSet<PeerId>,
    ) -> TestPeerProtocolHandles {
        let (protocol_tx, protocol_rx) = mpsc::unbounded_channel();
        let (replay_tx, replay_rx) = mpsc::channel(8);
        let session_activations = SessionActivationRegistry::default();
        self.install_session_activation_bridge(session_activations.clone());
        let state = HandlerSharedState::new_with_session_activations(
            protocol_tx,
            max_active_connections,
            trusted_peers.clone(),
            session_activations,
        );
        let handler = if node_role.is_main() {
            ZksProtocolHandler::<P, _>::for_main_node(replays, state)
        } else {
            ZksProtocolHandler::<P, _>::for_external_node(
                replays,
                ExternalNodeProtocolConfig {
                    starting_block: Arc::new(RwLock::new(starting_block)),
                    record_overrides,
                    max_blocks_per_message: 64,
                    // SYSCOIN: Test ENs must explicitly authorize the main-node RLPx identity.
                    trusted_main_node_peers: trusted_peers.into_iter().collect(),
                    replay_sender: replay_tx,
                    verification: None,
                    // Generous enough that the inactivity timeout never affects these tests.
                    replay_inactivity_timeout: std::time::Duration::from_secs(600),
                },
                state,
            )
        };
        self.add_rlpx_sub_protocol(handler);
        TestPeerProtocolHandles {
            protocol_rx,
            replay_rx,
            verify_result_rx: None,
            verify_batch_rx: None,
            outgoing_verify_results_tx: None,
            zks_2fa_registry: None,
        }
    }

    fn add_zks_2fa_sub_protocol(
        &mut self,
        node_role: NodeRole,
        starting_block: BlockNumber,
        replays: impl IntoIterator<Item = (BlockNumber, ReplayRecord)>,
        max_active_connections: (usize, usize),
        verifier_signing_key: Option<SecretString>,
        trusted_main_node_peers: HashSet<PeerId>,
    ) -> TestPeerProtocolHandles {
        // SYSCOIN: Bind verifier authentication to this exact local RLPx identity.
        let local_peer_id = self.peer_id();
        let (protocol_tx, protocol_rx) = mpsc::unbounded_channel();
        let (replay_tx, replay_rx) = mpsc::channel(8);
        // SYSCOIN: One accepted physical session releases both replay and verifier waiters.
        let session_activations = SessionActivationRegistry::default();
        self.install_session_activation_bridge(session_activations.clone());
        // Both subprotocols share one event stream, exactly as in production.
        let zks_state = HandlerSharedState::new_with_session_activations(
            protocol_tx.clone(),
            max_active_connections.0,
            trusted_main_node_peers.clone(),
            session_activations.clone(),
        );
        // SYSCOIN: Match production by applying trusted-peer admission to both independent caps.
        let twofa_state = HandlerSharedState::new_with_session_activations(
            protocol_tx,
            max_active_connections.1,
            trusted_main_node_peers.clone(),
            session_activations,
        );
        let zks_2fa_registry = Arc::new(RwLock::new(HashMap::new()));
        let replays = InMemReplay::new(replays);

        let (verify_result_rx, verify_batch_rx, outgoing_verify_results_tx) = if node_role.is_main()
        {
            let (verify_result_tx, verify_result_rx) = mpsc::channel(8);
            self.add_rlpx_sub_protocol(ZksProtocolHandler::<ZksProtocolV5, _>::for_main_node(
                replays, zks_state,
            ));
            self.add_rlpx_sub_protocol(Zks2faProtocolHandler::for_main_node(
                MainNode2faConfig {
                    chain_id: TEST_CHAIN_ID,
                    local_peer_id,
                    accepted_verifier_signers: accepted_verifier_signers(),
                    verify_result_tx,
                },
                twofa_state,
                zks_2fa_registry.clone(),
            ));
            (Some(verify_result_rx), None, None)
        } else {
            let signing_key =
                verifier_signing_key.expect("external verifier node requires a signing key");
            let trusted_main_node_peers: Vec<_> = trusted_main_node_peers.iter().copied().collect();
            let (verify_batch_tx, verify_batch_rx) = mpsc::channel(8);
            let (outgoing_verify_results, _outgoing_verify_results_rx) = broadcast::channel(8);
            self.add_rlpx_sub_protocol(ZksProtocolHandler::<ZksProtocolV5, _>::for_external_node(
                replays,
                ExternalNodeProtocolConfig {
                    starting_block: Arc::new(RwLock::new(starting_block)),
                    record_overrides: vec![],
                    max_blocks_per_message: 64,
                    // SYSCOIN: Test ENs must explicitly authorize the main-node RLPx identity.
                    trusted_main_node_peers: trusted_main_node_peers.clone(),
                    replay_sender: replay_tx,
                    verification: None,
                    // Generous enough that the inactivity timeout never affects these tests.
                    replay_inactivity_timeout: std::time::Duration::from_secs(600),
                },
                zks_state,
            ));
            self.add_rlpx_sub_protocol(Zks2faProtocolHandler::for_external_node(
                ExternalNode2faConfig {
                    chain_id: TEST_CHAIN_ID,
                    local_peer_id,
                    trusted_main_node_peers,
                    signing_key,
                    verify_batch_tx,
                    outgoing_verify_results: outgoing_verify_results.clone(),
                },
                twofa_state,
                zks_2fa_registry.clone(),
            ));
            (None, Some(verify_batch_rx), Some(outgoing_verify_results))
        };

        TestPeerProtocolHandles {
            protocol_rx,
            replay_rx,
            verify_result_rx,
            verify_batch_rx,
            outgoing_verify_results_tx,
            zks_2fa_registry: Some(zks_2fa_registry),
        }
    }
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn send_replay_record_matching_version() {
    // Run two peers that both communicate on exactly one matching zks protocol and successfully
    // transfer one replay record from peer0 to peer1.
    let mut net = Testnet::create_with(2, MockEthProvider::default()).await;
    let record1 = dummy_record::<ZksProtocolV5>(1);
    let main_peer_id = net.peers_mut()[0].peer_id();

    let (mut from_peer0, _) = net.peers_mut()[0].add_zks_sub_protocol::<ZksProtocolV5>(
        NodeRole::MainNode,
        0,
        [(1, record1.clone())],
        100,
        HashSet::new(),
    );
    let (mut from_peer1, mut replay_rx_peer1) = net.peers_mut()[1]
        .add_zks_sub_protocol::<ZksProtocolV5>(
            NodeRole::ExternalNode,
            1,
            [(1, record1.clone())],
            100,
            HashSet::from([main_peer_id]),
        );

    let handle = net.spawn();
    handle.connect_peers().await;

    assert_matches!(from_peer0.recv().await, Some(ProtocolEvent::Established { peer_id, .. }) => {
        assert_eq!(peer_id, *handle.peers()[1].peer_id());
    });
    assert_matches!(from_peer1.recv().await, Some(ProtocolEvent::Established { peer_id, .. }) => {
        assert_eq!(peer_id, *handle.peers()[0].peer_id());
    });

    let received_replay_record = replay_rx_peer1.recv().await.unwrap();
    assert_eq!(received_replay_record, record1);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn emits_replay_session_events() {
    let mut net = Testnet::create_with(2, MockEthProvider::default()).await;
    let record1 = dummy_record::<ZksProtocolV5>(1);
    let main_peer_id = net.peers_mut()[0].peer_id();

    let (mut from_peer0, _) = net.peers_mut()[0].add_zks_sub_protocol::<ZksProtocolV5>(
        NodeRole::MainNode,
        0,
        [(1, record1.clone())],
        100,
        HashSet::new(),
    );
    let (_, mut replay_rx_peer1) = net.peers_mut()[1].add_zks_sub_protocol::<ZksProtocolV5>(
        NodeRole::ExternalNode,
        1,
        [(1, record1.clone())],
        100,
        HashSet::from([main_peer_id]),
    );

    let handle = net.spawn();
    handle.connect_peers().await;

    let peer1_id = *handle.peers()[1].peer_id();
    let mut saw_established = false;
    let mut saw_replay_requested = false;
    let mut saw_replay_block_sent = false;

    while !(saw_established && saw_replay_requested && saw_replay_block_sent) {
        match from_peer0.recv().await {
            Some(ProtocolEvent::Established { peer_id, .. }) => {
                assert_eq!(peer_id, peer1_id);
                saw_established = true;
            }
            Some(ProtocolEvent::ReplayRequested {
                peer_id,
                starting_block,
            }) => {
                assert_eq!(peer_id, peer1_id);
                assert_eq!(starting_block, 1);
                saw_replay_requested = true;
            }
            Some(ProtocolEvent::ReplayBlockSent {
                peer_id,
                block_number,
            }) => {
                assert_eq!(peer_id, peer1_id);
                assert_eq!(block_number, 1);
                saw_replay_block_sent = true;
            }
            Some(ProtocolEvent::VerifierRoleRequested { .. }) => {
                panic!("unexpected verifier role request event")
            }
            Some(ProtocolEvent::Closed { .. }) => {}
            Some(
                ProtocolEvent::VerifierChallengeSent { .. }
                | ProtocolEvent::VerifierAuthorized { .. }
                | ProtocolEvent::VerifierUnauthorized { .. },
            ) => {}
            Some(ProtocolEvent::MaxActiveConnectionsExceeded { .. }) => {
                panic!("unexpected max active connections event")
            }
            Some(ProtocolEvent::ReplayStreamStalled { .. }) => {
                panic!("unexpected replay stream stall event")
            }
            None => panic!("protocol event stream closed before replay events were observed"),
        }
    }

    let received_replay_record = replay_rx_peer1.recv().await.unwrap();
    assert_eq!(received_replay_record, record1);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn external_node_rejects_replay_from_untrusted_peer() {
    let mut net = Testnet::create_with(2, MockEthProvider::default()).await;
    let record1 = dummy_record::<ZksProtocolV5>(1);
    let external_peer_id = net.peers_mut()[1].peer_id();
    let external_peer_addr = net.peers_mut()[1].local_addr();

    let (mut from_main, _) = net.peers_mut()[0].add_zks_sub_protocol::<ZksProtocolV5>(
        NodeRole::MainNode,
        0,
        [(1, record1)],
        100,
        HashSet::new(),
    );
    let (mut external_events, mut external_replay_rx) = net.peers_mut()[1]
        .add_zks_sub_protocol::<ZksProtocolV5>(
            NodeRole::ExternalNode,
            1,
            [],
            100,
            // SYSCOIN: Deliberately trust the wrong identity to exercise replay-source rejection.
            HashSet::from([external_peer_id]),
        );

    let handle = net.spawn();
    // `connect_peers()` waits for a persistent reth session, but the EN deliberately closes this
    // one as soon as the authenticated RLPx identity fails its replay-source allowlist.
    let mut main_network_events = handle.peers()[0].event_listener();
    handle.peers()[0]
        .network()
        .add_peer(external_peer_id, external_peer_addr);

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match main_network_events.next().await {
                Some(NetworkEvent::Peer(PeerEvent::PeerAdded(added))) => {
                    assert_eq!(added, external_peer_id);
                    break;
                }
                Some(_) => {}
                None => panic!("network event stream closed before the peer was added"),
            }
        }
    })
    .await
    .expect("untrusted-peer test dial was not registered");

    // SYSCOIN: Rejection occurs before the EN sends a replay request, which is the MN's mutual
    // stream proof. Neither endpoint may publish tentative lifecycle or replay side effects.
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(500),
            external_events.recv()
        )
        .await
        .is_err(),
        "untrusted replay source must not publish EN protocol lifecycle"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), from_main.recv())
            .await
            .is_err(),
        "untrusted replay source must not publish MN protocol lifecycle"
    );
    assert!(
        !matches!(
            tokio::time::timeout(
                std::time::Duration::from_millis(100),
                external_replay_rx.recv()
            )
            .await,
            Ok(Some(_))
        ),
        "untrusted peer must not receive a replay record"
    );
    assert_eq!(handle.peers()[0].network().num_connected_peers(), 0);
    assert_eq!(handle.peers()[1].network().num_connected_peers(), 0);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn serves_overridden_replay_records() {
    // Regression test for the reverted-block debugging flow (#657): an EN that requests record
    // overrides must be served the rows stored under the overridden db keys instead of the
    // canonical ones.
    let mut net = Testnet::create_with(2, MockEthProvider::default()).await;
    let main_peer_id = net.peers_mut()[0].peer_id();
    let record1 = dummy_record::<ZksProtocolV5>(1);
    let canonical2 = dummy_record::<ZksProtocolV5>(2);
    let reverted2 = dummy_record::<ZksProtocolV5>(2);
    assert_ne!(canonical2, reverted2);
    // Opaque to the protocol; in production this is the reverted block's hash.
    let db_key = vec![0xAB; 32];

    net.peers_mut()[0].add_zks_sub_protocol_with_storage::<ZksProtocolV5>(
        NodeRole::MainNode,
        0,
        InMemReplay::new([(1, record1.clone()), (2, canonical2.clone())])
            .with_override(db_key.clone(), reverted2.clone()),
        vec![],
        100,
        HashSet::new(),
    );
    let mut external = net.peers_mut()[1].add_zks_sub_protocol_with_storage::<ZksProtocolV5>(
        NodeRole::ExternalNode,
        1,
        InMemReplay::new([(1, record1.clone()), (2, canonical2.clone())]),
        vec![RecordOverride {
            block_number: 2,
            db_key: db_key.into(),
        }],
        100,
        HashSet::from([main_peer_id]),
    );

    let handle = net.spawn();
    handle.connect_peers().await;

    assert_eq!(external.replay_rx.recv().await.unwrap(), record1);
    assert_eq!(external.replay_rx.recv().await.unwrap(), reverted2);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn batches_multiple_replay_records() {
    let mut net = Testnet::create_with(2, MockEthProvider::default()).await;
    let record1 = dummy_record::<ZksProtocolV5>(1);
    let record2 = dummy_record::<ZksProtocolV5>(2);
    let main_peer_id = net.peers_mut()[0].peer_id();

    let (mut from_peer0, _) = net.peers_mut()[0].add_zks_sub_protocol::<ZksProtocolV5>(
        NodeRole::MainNode,
        0,
        [(1, record1.clone()), (2, record2.clone())],
        100,
        HashSet::new(),
    );
    let (_, mut replay_rx_peer1) = net.peers_mut()[1].add_zks_sub_protocol::<ZksProtocolV5>(
        NodeRole::ExternalNode,
        1,
        [(1, record1.clone()), (2, record2.clone())],
        100,
        HashSet::from([main_peer_id]),
    );

    let handle = net.spawn();
    handle.connect_peers().await;

    let peer1_id = *handle.peers()[1].peer_id();
    let mut replay_blocks_sent = Vec::new();
    while replay_blocks_sent.len() < 2 {
        match from_peer0.recv().await {
            Some(ProtocolEvent::ReplayBlockSent {
                peer_id,
                block_number,
            }) => {
                assert_eq!(peer_id, peer1_id);
                replay_blocks_sent.push(block_number);
            }
            Some(
                ProtocolEvent::Established { .. }
                | ProtocolEvent::ReplayRequested { .. }
                | ProtocolEvent::Closed { .. },
            ) => {}
            Some(
                ProtocolEvent::VerifierRoleRequested { .. }
                | ProtocolEvent::VerifierChallengeSent { .. }
                | ProtocolEvent::VerifierAuthorized { .. }
                | ProtocolEvent::VerifierUnauthorized { .. },
            ) => panic!("unexpected verifier event during replay batching test"),
            Some(ProtocolEvent::MaxActiveConnectionsExceeded { .. }) => {
                panic!("unexpected max active connections event")
            }
            Some(ProtocolEvent::ReplayStreamStalled { .. }) => {
                panic!("unexpected replay stream stall event")
            }
            None => {
                panic!("protocol event stream closed before batched replay events were observed")
            }
        }
    }

    assert_eq!(replay_blocks_sent, vec![1, 2]);
    assert_eq!(replay_rx_peer1.recv().await.unwrap(), record1);
    assert_eq!(replay_rx_peer1.recv().await.unwrap(), record2);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn send_replay_record_different_versions() {
    // Peer 0 advertises `zks/0` and `zks/5`, while peer 1 advertises only `zks/0`. They must select
    // `zks/0`, whose replay record preserves only the block number.
    let mut net = Testnet::create_with(2, MockEthProvider::default()).await;
    let record1 = dummy_record::<ZksProtocolV5>(1);
    let main_peer_id = net.peers_mut()[0].peer_id();
    let (_, _) = net.peers_mut()[0].add_zks_sub_protocol::<ZksProtocolV5>(
        NodeRole::MainNode,
        0,
        [(1, record1.clone())],
        100,
        HashSet::new(),
    );
    let (mut from_peer0, _) = net.peers_mut()[0].add_zks_sub_protocol::<ZksProtocolV0>(
        NodeRole::MainNode,
        0,
        [(1, record1.clone())],
        100,
        HashSet::new(),
    );

    let (mut from_peer1, mut replay_rx_peer1) = net.peers_mut()[1]
        .add_zks_sub_protocol::<ZksProtocolV0>(
            NodeRole::ExternalNode,
            1,
            [(1, record1.clone())],
            100,
            HashSet::from([main_peer_id]),
        );

    let handle = net.spawn();
    handle.connect_peers().await;

    assert_matches!(from_peer0.recv().await, Some(ProtocolEvent::Established { peer_id, .. }) => {
        assert_eq!(peer_id, *handle.peers()[1].peer_id());
    });
    assert_matches!(from_peer1.recv().await, Some(ProtocolEvent::Established { peer_id, .. }) => {
        assert_eq!(peer_id, *handle.peers()[0].peer_id());
    });

    let received_replay_record = replay_rx_peer1.recv().await.unwrap();
    // The negotiated v0 format deliberately discards every other field.
    assert_ne!(received_replay_record, record1);
    assert_eq!(
        received_replay_record.block_context.block_number,
        record1.block_context.block_number
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn disconnects_peer_without_common_zks_version() {
    // Peer 0 speaks only `zks/5` and peer 1 only `zks/0`, so capability negotiation yields no
    // shared `zks` version. The handler must drop the whole session during capability negotiation
    // of letting a never-syncing peer occupy a connection slot.
    let mut net = Testnet::create_with(2, MockEthProvider::default()).await;
    let main_peer_id = net.peers_mut()[0].peer_id();

    let (mut from_peer0, _) = net.peers_mut()[0].add_zks_sub_protocol::<ZksProtocolV5>(
        NodeRole::MainNode,
        0,
        [],
        100,
        HashSet::new(),
    );
    let peer1 = &mut net.peers_mut()[1];
    let peer1_id = peer1.peer_id();
    let peer1_addr = peer1.local_addr();
    let (mut from_peer1, _) = peer1.add_zks_sub_protocol::<ZksProtocolV0>(
        NodeRole::ExternalNode,
        1,
        [],
        100,
        HashSet::from([main_peer_id]),
    );

    let handle = net.spawn();
    // `connect_peers()` would hang here: it waits for sessions that never establish. Dial
    // manually instead.
    let mut peer0_events = handle.peers()[0].event_listener();
    handle.peers()[0].network().add_peer(peer1_id, peer1_addr);

    // Confirm the dial was registered so the negative assertions below cannot pass vacuously.
    loop {
        match peer0_events.next().await {
            Some(NetworkEvent::Peer(PeerEvent::PeerAdded(added))) => {
                assert_eq!(added, peer1_id);
                break;
            }
            Some(_) => {}
            None => panic!("network event stream closed before the peer was added"),
        }
    }

    // Rejection happens before either handler can emit `ProtocolEvent::Established` and before reth
    // counts the peer as connected.
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), from_peer0.recv())
            .await
            .is_err(),
        "peer0 must not establish a zks connection with a version-disjoint peer"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), from_peer1.recv())
            .await
            .is_err(),
        "peer1 must not establish a zks connection with a version-disjoint peer"
    );
    assert_eq!(handle.peers()[0].network().num_connected_peers(), 0);
    assert_eq!(handle.peers()[1].network().num_connected_peers(), 0);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn max_active_connections() {
    // Run three peers while peer0 has max active connections set to 1. peer1 is expected to be
    // successfully connected first while peer2 is expected to error out with
    // `MaxActiveConnectionsExceeded`.
    let mut net = Testnet::create_with(3, MockEthProvider::default()).await;
    let main_peer_id = net.peers_mut()[0].peer_id();

    let (mut from_peer0, _) = net.peers_mut()[0].add_zks_sub_protocol::<ZksProtocolV5>(
        NodeRole::MainNode,
        1,
        [],
        1,
        HashSet::new(),
    );

    let peer1 = &mut net.peers_mut()[1];
    let peer1_id = peer1.peer_id();
    let peer1_addr = peer1.local_addr();
    let (_, _) = peer1.add_zks_sub_protocol::<ZksProtocolV5>(
        NodeRole::ExternalNode,
        1,
        [],
        100,
        HashSet::from([main_peer_id]),
    );

    let peer2 = &mut net.peers_mut()[2];
    let peer2_id = peer2.peer_id();
    let peer2_addr = peer2.local_addr();
    let (_, _) = peer2.add_zks_sub_protocol::<ZksProtocolV5>(
        NodeRole::ExternalNode,
        1,
        [],
        100,
        HashSet::from([main_peer_id]),
    );

    let handle = net.spawn();

    // Connect peers 0 and 1
    let peer0_handle = &handle.peers()[0];
    peer0_handle.network().add_peer(peer1_id, peer1_addr);
    assert_matches!(from_peer0.recv().await, Some(ProtocolEvent::Established { peer_id, .. }) => {
        assert_eq!(peer_id, *peer1_id);
    });

    // Connect peers 0 and 2, max active connections exceeded
    peer0_handle.network().add_peer(peer2_id, peer2_addr);
    loop {
        match from_peer0.recv().await {
            Some(ProtocolEvent::MaxActiveConnectionsExceeded { max_connections }) => {
                assert_eq!(max_connections, 1);
                break;
            }
            Some(
                ProtocolEvent::ReplayRequested { .. }
                | ProtocolEvent::ReplayBlockSent { .. }
                | ProtocolEvent::VerifierRoleRequested { .. }
                | ProtocolEvent::VerifierChallengeSent { .. }
                | ProtocolEvent::VerifierAuthorized { .. }
                | ProtocolEvent::VerifierUnauthorized { .. },
            ) => {}
            other => panic!("unexpected protocol event: {other:?}"),
        }
    }
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn trusted_peer_bypasses_max_active_connections() {
    // peer0 (main node) allows zero active connections, so only a trusted peer can connect. Its
    // outgoing dial to trusted peer1 is admitted regardless of the cap.
    let mut net = Testnet::create_with(2, MockEthProvider::default()).await;
    let main_peer_id = net.peers_mut()[0].peer_id();
    let peer1_id = net.peers_mut()[1].peer_id();

    let mut from_peer0 = net.peers_mut()[0]
        .add_zks_sub_protocol_with_test_handles::<ZksProtocolV5>(
            NodeRole::MainNode,
            1,
            [],
            0,
            HashSet::from([peer1_id]),
        )
        .protocol_rx;
    let peer1_addr = net.peers_mut()[1].local_addr();
    net.peers_mut()[1].add_zks_sub_protocol::<ZksProtocolV5>(
        NodeRole::ExternalNode,
        1,
        [],
        100,
        HashSet::from([main_peer_id]),
    );

    let handle = net.spawn();
    handle.peers()[0].network().add_peer(peer1_id, peer1_addr);

    assert_matches!(from_peer0.recv().await, Some(ProtocolEvent::Established { peer_id, .. }) => {
        assert_eq!(peer_id, peer1_id);
    });
}

// SYSCOIN: Deferred incoming admission must still enforce the mandatory replay cap for an
// untrusted PeerId, and a rejected tentative handler must publish no session lifecycle state.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn untrusted_incoming_peer_at_zero_cap_has_no_replay_lifecycle_events() {
    let mut net = Testnet::create_with(2, MockEthProvider::default()).await;
    let main_peer_id = net.peers_mut()[0].peer_id();
    let main_addr = net.peers_mut()[0].local_addr();
    let external_peer_id = net.peers_mut()[1].peer_id();
    let mut main_events = net.peers_mut()[0]
        .add_zks_sub_protocol_with_test_handles::<ZksProtocolV5>(
            NodeRole::MainNode,
            1,
            [],
            0,
            HashSet::new(),
        )
        .protocol_rx;
    net.peers_mut()[1].add_zks_sub_protocol::<ZksProtocolV5>(
        NodeRole::ExternalNode,
        1,
        [],
        100,
        HashSet::from([main_peer_id]),
    );

    let handle = net.spawn();
    handle.peers()[1]
        .network()
        .add_peer(main_peer_id, main_addr);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match main_events.recv().await {
                Some(ProtocolEvent::MaxActiveConnectionsExceeded { max_connections }) => {
                    assert_eq!(max_connections, 0);
                    break;
                }
                Some(ProtocolEvent::Established { .. } | ProtocolEvent::ReplayRequested { .. }) => {
                    panic!("cap-rejected replay handler emitted lifecycle state")
                }
                Some(_) => {}
                None => panic!("main event stream closed before cap rejection"),
            }
        }
    })
    .await
    .expect("incoming untrusted peer did not reach deferred cap admission");

    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    while let Ok(event) = main_events.try_recv() {
        assert!(
            !matches!(
                event,
                ProtocolEvent::Established { peer_id, .. }
                    | ProtocolEvent::ReplayRequested { peer_id, .. }
                    if peer_id == external_peer_id
            ),
            "rejected untrusted peer must not appear in replay lifecycle state"
        );
    }
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn zks_2fa_authorizes_verifier_and_replays() {
    // A trusted verifier uses independent lanes: replay over `zks/5` and authentication over
    // `zks_2fa`. The verifier lane must bypass its full cap while replay also makes progress on the
    // same RLPx connection.
    let mut net = Testnet::create_with(2, MockEthProvider::default()).await;
    let record1 = dummy_record::<ZksProtocolV5>(1);
    let main_peer_id = net.peers_mut()[0].peer_id();
    let main_addr = net.peers_mut()[0].local_addr();
    let external_peer_id = net.peers_mut()[1].peer_id();
    let expected_signer = PrivateKeySigner::from_str(
        "0x7726827caac94a7f9e1b160f7ea819f172f7b6f9d2a97f992c38edeab82d4110",
    )
    .unwrap()
    .address();

    let mut main = net.peers_mut()[0].add_zks_2fa_sub_protocol(
        NodeRole::MainNode,
        0,
        [(1, record1.clone())],
        (0, 0),
        None,
        HashSet::from([external_peer_id]),
    );
    let mut external = net.peers_mut()[1].add_zks_2fa_sub_protocol(
        NodeRole::ExternalNode,
        1,
        [(1, record1.clone())],
        (100, 100),
        Some(default_verifier_signing_key()),
        HashSet::from([main_peer_id]),
    );

    let handle = net.spawn();
    // Verifier ENs initiate the production connection; the main node learns their ID after RLPx.
    handle.peers()[1]
        .network()
        .add_peer(main_peer_id, main_addr);

    let peer1_id = *handle.peers()[1].peer_id();
    let mut saw_verifier_authorized = false;
    let mut saw_replay_requested = false;

    while !(saw_verifier_authorized && saw_replay_requested) {
        match main.protocol_rx.recv().await {
            Some(ProtocolEvent::VerifierAuthorized {
                peer_id, signer, ..
            }) => {
                assert_eq!(peer_id, peer1_id);
                assert_eq!(signer, expected_signer);
                saw_verifier_authorized = true;
            }
            Some(ProtocolEvent::ReplayRequested {
                peer_id,
                starting_block,
            }) => {
                assert_eq!(peer_id, peer1_id);
                assert_eq!(starting_block, 1);
                saw_replay_requested = true;
            }
            Some(ProtocolEvent::VerifierUnauthorized { signer, .. }) => {
                panic!("unexpected verifier unauthorized event: {signer:?}")
            }
            Some(
                ProtocolEvent::Established { .. }
                | ProtocolEvent::Closed { .. }
                | ProtocolEvent::ReplayBlockSent { .. }
                | ProtocolEvent::VerifierRoleRequested { .. }
                | ProtocolEvent::VerifierChallengeSent { .. },
            ) => {}
            Some(ProtocolEvent::MaxActiveConnectionsExceeded { .. }) => {
                panic!("unexpected max active connections event")
            }
            Some(ProtocolEvent::ReplayStreamStalled { .. }) => {
                panic!("unexpected replay stream stall event")
            }
            None => panic!("event stream closed before verifier auth + replay were observed"),
        }
    }

    let received_replay_record = external.replay_rx.recv().await.unwrap();
    assert_eq!(received_replay_record, record1);
}

// SYSCOIN: Both sides may dial a boot peer at the same time. Reth constructs both protocol
// handlers before rejecting one RLPx duplicate, so loser teardown must not emit replay lifecycle
// events or replace the accepted verifier lane.
#[test_log::test(tokio::test(flavor = "multi_thread", worker_threads = 4))]
async fn simultaneous_dial_keeps_first_exact_replay_and_verifier_owners() {
    let mut net = Testnet::create_with(2, MockEthProvider::default()).await;
    let record1 = dummy_record::<ZksProtocolV5>(1);
    let main_peer_id = net.peers_mut()[0].peer_id();
    let main_addr = net.peers_mut()[0].local_addr();
    let external_peer_id = net.peers_mut()[1].peer_id();
    let external_addr = net.peers_mut()[1].local_addr();

    let mut main = net.peers_mut()[0].add_zks_2fa_sub_protocol(
        NodeRole::MainNode,
        0,
        [(1, record1.clone())],
        (0, 0),
        None,
        HashSet::from([external_peer_id]),
    );
    let mut external = net.peers_mut()[1].add_zks_2fa_sub_protocol(
        NodeRole::ExternalNode,
        1,
        [(1, record1.clone())],
        (0, 0),
        Some(default_verifier_signing_key()),
        HashSet::from([main_peer_id]),
    );

    let handle = net.spawn();
    let main_network = handle.peers()[0].network().clone();
    let external_network = handle.peers()[1].network().clone();
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let main_barrier = Arc::clone(&barrier);
    let main_dial = tokio::spawn(async move {
        main_barrier.wait().await;
        main_network.add_peer(external_peer_id, external_addr);
    });
    let external_barrier = Arc::clone(&barrier);
    let external_dial = tokio::spawn(async move {
        external_barrier.wait().await;
        external_network.add_peer(main_peer_id, main_addr);
    });
    barrier.wait().await;
    main_dial.await.unwrap();
    external_dial.await.unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut authorized = false;
        let mut replay_requested = false;
        while !(authorized && replay_requested) {
            match main.protocol_rx.recv().await {
                Some(ProtocolEvent::VerifierAuthorized { peer_id, .. }) => {
                    assert_eq!(peer_id, external_peer_id);
                    authorized = true;
                }
                Some(ProtocolEvent::ReplayRequested { peer_id, .. }) => {
                    assert_eq!(peer_id, external_peer_id);
                    replay_requested = true;
                }
                Some(ProtocolEvent::Closed { peer_id }) if peer_id == external_peer_id => {
                    panic!("tentative duplicate emitted a replay Closed event")
                }
                Some(_) => {}
                None => panic!("event stream closed during simultaneous-dial authentication"),
            }
        }
    })
    .await
    .expect("simultaneous dials did not converge on one authenticated owner");

    let lane = main
        .zks_2fa_registry
        .as_ref()
        .unwrap()
        .read()
        .unwrap()
        .get(&external_peer_id)
        .expect("accepted simultaneous-dial lane is registered")
        .clone();
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    while let Ok(event) = main.protocol_rx.try_recv() {
        assert!(
            !matches!(event, ProtocolEvent::Closed { peer_id } if peer_id == external_peer_id),
            "duplicate teardown must not close the accepted replay lifecycle"
        );
    }
    while let Ok(event) = external.protocol_rx.try_recv() {
        assert!(
            !matches!(event, ProtocolEvent::Closed { peer_id } if peer_id == main_peer_id),
            "duplicate teardown must not close the accepted reverse replay lifecycle"
        );
    }
    assert!(
        main.zks_2fa_registry
            .as_ref()
            .unwrap()
            .read()
            .unwrap()
            .contains_key(&external_peer_id),
        "accepted lane survives duplicate teardown"
    );
    assert!(
        external
            .zks_2fa_registry
            .as_ref()
            .unwrap()
            .read()
            .unwrap()
            .contains_key(&main_peer_id),
        "accepted reverse lane survives duplicate teardown"
    );

    let request = dummy_verify_batch();
    lane.try_send_verify_batch(
        request.clone(),
        tokio::time::Instant::now() + TEST_VERIFY_REQUEST_TIMEOUT,
    )
    .unwrap();
    let admitted = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        external.verify_batch_rx.as_mut().unwrap().recv(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(admitted.message, request);
    assert_eq!(external.replay_rx.recv().await.unwrap(), record1);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn zks_2fa_cap_rejection_keeps_replay_connected() {
    let mut net = Testnet::create_with(2, MockEthProvider::default()).await;
    let record1 = dummy_record::<ZksProtocolV5>(1);
    let main_peer_id = net.peers_mut()[0].peer_id();
    let main_addr = net.peers_mut()[0].local_addr();

    let mut main = net.peers_mut()[0].add_zks_2fa_sub_protocol(
        NodeRole::MainNode,
        0,
        [(1, record1.clone())],
        (100, 0),
        None,
        HashSet::new(),
    );
    let mut external = net.peers_mut()[1].add_zks_2fa_sub_protocol(
        NodeRole::ExternalNode,
        1,
        [(1, record1.clone())],
        (100, 100),
        Some(default_verifier_signing_key()),
        HashSet::from([main_peer_id]),
    );

    let handle = net.spawn();
    handle.peers()[1]
        .network()
        .add_peer(main_peer_id, main_addr);

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut saw_cap_rejection = false;
        let mut saw_replay_request = false;
        while !(saw_cap_rejection && saw_replay_request) {
            match main.protocol_rx.recv().await {
                Some(ProtocolEvent::MaxActiveConnectionsExceeded { max_connections }) => {
                    assert_eq!(max_connections, 0);
                    saw_cap_rejection = true;
                }
                Some(ProtocolEvent::ReplayRequested { starting_block, .. }) => {
                    assert_eq!(starting_block, 1);
                    saw_replay_request = true;
                }
                Some(_) => {}
                None => panic!("event stream closed before 2FA rejection and replay"),
            }
        }
    })
    .await
    .expect("2FA cap rejection must not close replay");

    let received_replay_record = external.replay_rx.recv().await.unwrap();
    assert_eq!(received_replay_record, record1);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn zks_2fa_emits_verifier_unauthorized() {
    // A verifier EN authenticating with a signer the main node does not accept must be reported
    // as unauthorized over `zks_2fa`, while replay still proceeds over the independent `zks/5`
    // lane.
    let mut net = Testnet::create_with(2, MockEthProvider::default()).await;
    let record1 = dummy_record::<ZksProtocolV5>(1);
    let main_peer_id = net.peers_mut()[0].peer_id();

    let mut main = net.peers_mut()[0].add_zks_2fa_sub_protocol(
        NodeRole::MainNode,
        0,
        [(1, record1.clone())],
        (100, 100),
        None,
        HashSet::new(),
    );
    let mut external = net.peers_mut()[1].add_zks_2fa_sub_protocol(
        NodeRole::ExternalNode,
        1,
        [(1, record1.clone())],
        (100, 100),
        Some(alternate_verifier_signing_key()),
        HashSet::from([main_peer_id]),
    );

    let handle = net.spawn();
    handle.connect_peers().await;

    let peer1_id = *handle.peers()[1].peer_id();
    let mut saw_verifier_unauthorized = false;
    let mut saw_replay_requested = false;

    while !(saw_verifier_unauthorized && saw_replay_requested) {
        match main.protocol_rx.recv().await {
            Some(ProtocolEvent::VerifierUnauthorized {
                peer_id, signer, ..
            }) => {
                assert_eq!(peer_id, peer1_id);
                assert!(signer.is_some());
                saw_verifier_unauthorized = true;
            }
            Some(ProtocolEvent::ReplayRequested {
                peer_id,
                starting_block,
            }) => {
                assert_eq!(peer_id, peer1_id);
                assert_eq!(starting_block, 1);
                saw_replay_requested = true;
            }
            Some(ProtocolEvent::VerifierAuthorized { signer, .. }) => {
                panic!("unexpected verifier authorized event: {signer:?}")
            }
            Some(
                ProtocolEvent::Established { .. }
                | ProtocolEvent::Closed { .. }
                | ProtocolEvent::ReplayBlockSent { .. }
                | ProtocolEvent::VerifierRoleRequested { .. }
                | ProtocolEvent::VerifierChallengeSent { .. },
            ) => {}
            Some(ProtocolEvent::MaxActiveConnectionsExceeded { .. }) => {
                panic!("unexpected max active connections event")
            }
            Some(ProtocolEvent::ReplayStreamStalled { .. }) => {
                panic!("unexpected replay stream stall event")
            }
            None => panic!("event stream closed before verifier auth failure was observed"),
        }
    }

    let received_replay_record = external.replay_rx.recv().await.unwrap();
    assert_eq!(received_replay_record, record1);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn zks_2fa_forwards_owned_verify_batch_result_and_keeps_replay() {
    let mut net = Testnet::create_with(2, MockEthProvider::default()).await;
    let record1 = dummy_record::<ZksProtocolV5>(1);
    let configured_main_peer_id = net.peers_mut()[0].peer_id();
    let main_addr = net.peers_mut()[0].local_addr();
    let external_peer_id = net.peers_mut()[1].peer_id();

    let mut main = net.peers_mut()[0].add_zks_2fa_sub_protocol(
        NodeRole::MainNode,
        0,
        [(1, record1.clone())],
        // SYSCOIN: Both mandatory replay and optional verifier admission are deferred until the
        // incoming EN PeerId is known, so this trusted EN must bypass both zero-sized caps.
        (0, 0),
        None,
        HashSet::from([external_peer_id]),
    );
    let mut external = net.peers_mut()[1].add_zks_2fa_sub_protocol(
        NodeRole::ExternalNode,
        1,
        [(1, record1.clone())],
        (100, 100),
        Some(default_verifier_signing_key()),
        HashSet::from([configured_main_peer_id]),
    );

    let handle = net.spawn();
    // SYSCOIN: Exercise the production direction explicitly: the verifier EN dials the MN.
    handle.peers()[1]
        .network()
        .add_peer(configured_main_peer_id, main_addr);

    let main_peer_id = *handle.peers()[0].peer_id();
    let external_peer_id = *handle.peers()[1].peer_id();
    loop {
        match main.protocol_rx.recv().await {
            Some(ProtocolEvent::VerifierAuthorized { peer_id, .. }) => {
                assert_eq!(peer_id, external_peer_id);
                break;
            }
            Some(ProtocolEvent::VerifierUnauthorized { signer, .. }) => {
                panic!("unexpected verifier unauthorized event: {signer:?}")
            }
            Some(_) => {}
            None => panic!("event stream closed before verifier was authorized"),
        }
    }

    let main_lane = main
        .zks_2fa_registry
        .as_ref()
        .unwrap()
        .read()
        .unwrap()
        .get(&external_peer_id)
        .expect("authenticated verifier lane is registered")
        .clone();
    let request = dummy_verify_batch();
    main_lane
        .try_send_verify_batch(
            request.clone(),
            tokio::time::Instant::now() + TEST_VERIFY_REQUEST_TIMEOUT,
        )
        .expect("owned request dispatch succeeds");

    let admitted = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        external.verify_batch_rx.as_mut().unwrap().recv(),
    )
    .await
    .expect("EN did not receive the transported request")
    .expect("EN verifier request channel closed");
    assert_eq!(admitted.peer_id, main_peer_id);
    assert_ne!(admitted.lane_id, 0);
    assert_eq!(admitted.message, request);

    let expected = dummy_verify_batch_result();
    external
        .outgoing_verify_results_tx
        .as_ref()
        .unwrap()
        .send(PeerVerifyBatchResult {
            peer_id: admitted.peer_id,
            lane_id: admitted.lane_id,
            message: expected.clone(),
        })
        .unwrap();

    let forwarded = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        main.verify_result_rx.as_mut().unwrap().recv(),
    )
    .await
    .expect("MN did not receive the transported result")
    .expect("MN verifier result channel closed");
    assert_eq!(forwarded.peer_id, external_peer_id);
    assert_ne!(forwarded.lane_id, 0);
    assert_eq!(forwarded.message, expected);

    let received_replay_record =
        tokio::time::timeout(std::time::Duration::from_secs(5), external.replay_rx.recv())
            .await
            .expect("owned verification traffic must not close replay")
            .unwrap();
    assert_eq!(received_replay_record, record1);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn zks_2fa_drops_unowned_local_result_without_closing_replay() {
    let mut net = Testnet::create_with(2, MockEthProvider::default()).await;
    let record1 = dummy_record::<ZksProtocolV5>(1);
    let configured_main_peer_id = net.peers_mut()[0].peer_id();

    let mut main = net.peers_mut()[0].add_zks_2fa_sub_protocol(
        NodeRole::MainNode,
        0,
        [(1, record1.clone())],
        (100, 100),
        None,
        HashSet::new(),
    );
    let mut external = net.peers_mut()[1].add_zks_2fa_sub_protocol(
        NodeRole::ExternalNode,
        1,
        [(1, record1.clone())],
        (100, 100),
        Some(default_verifier_signing_key()),
        HashSet::from([configured_main_peer_id]),
    );

    let handle = net.spawn();
    handle.connect_peers().await;

    let main_peer_id = *handle.peers()[0].peer_id();
    let external_peer_id = *handle.peers()[1].peer_id();

    // Wait for authentication to complete before injecting a local result that is not owned by
    // the live EN connection generation.
    loop {
        match main.protocol_rx.recv().await {
            Some(ProtocolEvent::VerifierAuthorized { peer_id, .. }) => {
                assert_eq!(peer_id, external_peer_id);
                break;
            }
            Some(ProtocolEvent::VerifierUnauthorized { signer, .. }) => {
                panic!("unexpected verifier unauthorized event: {signer:?}")
            }
            Some(_) => {}
            None => panic!("event stream closed before verifier was authorized"),
        }
    }

    let result = dummy_verify_batch_result();
    external
        .outgoing_verify_results_tx
        .as_ref()
        .unwrap()
        .send(PeerVerifyBatchResult {
            peer_id: main_peer_id,
            lane_id: 0,
            message: result.clone(),
        })
        .unwrap();

    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(250),
            main.verify_result_rx.as_mut().unwrap().recv(),
        )
        .await
        .is_err(),
        "a result not owned by the live EN lane must not enter the main-node channel"
    );

    let received_replay_record =
        tokio::time::timeout(std::time::Duration::from_secs(5), external.replay_rx.recv())
            .await
            .expect("dropping an unowned local result must not close replay")
            .unwrap();
    assert_eq!(received_replay_record, record1);
}
