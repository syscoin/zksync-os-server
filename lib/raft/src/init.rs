use crate::config::RaftConsensusConfig;
use crate::model::{
    BlockCanonizationEngine, ConsensusBootstrapper, ConsensusNetworkProtocol, ConsensusRole,
    ConsensusRuntimeParts, ConsensusStatusSource, LeadershipSignal, OpenRaftCanonizationEngine,
};
use crate::network::{RaftNetworkFactory, RaftRpcHandler};
use crate::state_machine::RaftStateMachineStore;
use crate::status::RaftConsensusStatus;
use crate::storage::RaftLogStore;
use anyhow::Context;
use openraft::{Config, Raft, SnapshotPolicy};
use reth_network_peers::PeerId;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::time::timeout;
use zksync_os_consensus_types::{RaftNode, RaftTypeConfig};
use zksync_os_network::raft::protocol::RaftProtocolHandler;
use zksync_os_network::raft::protocol::RaftRouter;
use zksync_os_sequencer::execution::NoopCanonization;
pub async fn init_consensus(config: RaftConsensusConfig) -> anyhow::Result<ConsensusRuntimeParts> {
    anyhow::ensure!(
        config.peer_ids.contains(&config.node_id),
        "consensus.peer_ids does not include local peer id derived from network.secret_key: {}",
        config.node_id
    );

    let router = RaftRouter::default();
    let node_id = config.node_id;
    let raft_config = Config {
        cluster_name: "zksync-os-server".to_owned(),
        snapshot_policy: SnapshotPolicy::Never,
        election_timeout_max: config.election_timeout_max.as_millis() as u64,
        election_timeout_min: config.election_timeout_min.as_millis() as u64,
        heartbeat_interval: config.heartbeat_interval.as_millis() as u64,
        ..Default::default()
    };

    let raft_config = Arc::new(raft_config.validate().context("invalid raft config")?);

    let log_store = RaftLogStore::open(&config.storage_path)?;
    let (canonized_sender, canonized_rx) = mpsc::channel(CANONIZED_BUFFER_SIZE);
    let state_machine = RaftStateMachineStore::new(log_store.db(), canonized_sender);

    let nodes = peer_list_to_nodes(&config.peer_ids);
    let membership_nodes = nodes
        .iter()
        .map(|(id, node)| (*id, node.clone()))
        .collect::<BTreeMap<_, _>>();
    let peer_ids: Vec<_> = membership_nodes.keys().copied().collect();
    tracing::info!(
        %node_id,
        peers_count = config.peer_ids.len(),
        bootstrap = config.bootstrap,
        ?peer_ids,
        "creating openraft consensus"
    );
    let network_factory = RaftNetworkFactory::new(router.clone(), &nodes, raft_config.as_ref())
        .context("build raft network factory")?;

    let raft = Raft::new(
        config.node_id,
        raft_config,
        network_factory,
        log_store,
        state_machine,
    )
    .await?;
    tracing::info!(%node_id, "openraft runtime created");
    let (leader_tx, leader_rx) = watch::channel(ConsensusRole::Replica);
    let (status_tx, status_rx) = watch::channel(RaftConsensusStatus {
        node_id: node_id.to_string(),
        state: "Learner".to_owned(),
        is_leader: false,
        current_leader: None,
        current_term: 0,
        last_applied_index: None,
    });
    spawn_metrics_task(raft.clone(), node_id.to_string(), leader_tx, status_tx);
    let rpc_handler = RaftRpcHandler::new(raft.clone());
    let protocol_handler = RaftProtocolHandler::new(Arc::new(rpc_handler), router.clone());

    Ok(ConsensusRuntimeParts {
        canonization_engine: BlockCanonizationEngine::OpenRaft(OpenRaftCanonizationEngine {
            raft: raft.clone(),
            canonized_blocks_rx: canonized_rx,
        }),
        leadership: LeadershipSignal::Watch(leader_rx),
        network_protocol: ConsensusNetworkProtocol::Raft(protocol_handler),
        bootstrapper: ConsensusBootstrapper::Raft(crate::bootstrap::RaftBootstrapper {
            raft: raft.clone(),
            bootstrap: config.bootstrap,
            router,
            node_id,
            peer_ids,
            membership_nodes,
        }),
        status: ConsensusStatusSource::Raft(status_rx),
    })
}

pub fn loopback_consensus() -> ConsensusRuntimeParts {
    ConsensusRuntimeParts {
        canonization_engine: BlockCanonizationEngine::Noop(NoopCanonization::new()),
        leadership: LeadershipSignal::AlwaysLeader,
        network_protocol: ConsensusNetworkProtocol::Disabled,
        bootstrapper: ConsensusBootstrapper::Noop,
        status: ConsensusStatusSource::None,
    }
}

const CANONIZED_BUFFER_SIZE: usize = 8;

fn spawn_metrics_task(
    raft: Raft<RaftTypeConfig>,
    node_id_str: String,
    leader_tx: watch::Sender<ConsensusRole>,
    status_tx: watch::Sender<RaftConsensusStatus>,
) {
    let raft_for_leader_check = raft.clone();
    let mut metrics_rx = raft.metrics();
    tokio::spawn(async move {
        #[derive(Debug, Clone, PartialEq, Eq)]
        struct MetricsSnapshot {
            state: openraft::ServerState,
            current_term: u64,
            vote: openraft::Vote<PeerId>,
            last_log_index: Option<u64>,
            last_applied: Option<openraft::LogId<PeerId>>,
            current_leader: Option<PeerId>,
        }

        let mut last_snapshot: Option<MetricsSnapshot> = None;
        let mut last_is_leader = None;
        let mut leader_confirmed = false;
        let mut last_claims_leader = false;

        loop {
            if metrics_rx.changed().await.is_err() {
                break;
            }
            let metrics = metrics_rx.borrow().clone();
            let snapshot = MetricsSnapshot {
                state: metrics.state,
                current_term: metrics.current_term,
                vote: metrics.vote,
                last_log_index: metrics.last_log_index,
                last_applied: metrics.last_applied,
                current_leader: metrics.current_leader,
            };
            if last_snapshot.as_ref() != Some(&snapshot) {
                tracing::debug!(?snapshot, "OpenRaft metrics changed");
                last_snapshot = Some(snapshot);
            }

            let claims_leader = matches!(metrics.state, openraft::ServerState::Leader);
            if !claims_leader {
                leader_confirmed = false;
            } else if !last_claims_leader {
                leader_confirmed = match timeout(
                    Duration::from_secs(2),
                    raft_for_leader_check.ensure_linearizable(),
                )
                .await
                {
                    Ok(Ok(_)) => {
                        tracing::info!("OpenRaft leader confirmation succeeded");
                        true
                    }
                    Ok(Err(err)) => {
                        tracing::warn!(%err, "OpenRaft leader confirmation failed");
                        false
                    }
                    Err(_) => {
                        tracing::warn!(
                            "OpenRaft leader confirmation timed out while state=Leader"
                        );
                        false
                    }
                };
            }
            last_claims_leader = claims_leader;

            let role = if claims_leader && leader_confirmed {
                ConsensusRole::Leader
            } else {
                ConsensusRole::Replica
            };
            if last_is_leader != Some(role == ConsensusRole::Leader) {
                tracing::info!(role = ?role, "OpenRaft leadership status changed");
                last_is_leader = Some(role == ConsensusRole::Leader);
            }
            let status = RaftConsensusStatus {
                node_id: node_id_str.clone(),
                state: format!("{:?}", metrics.state),
                is_leader: role == ConsensusRole::Leader,
                current_leader: metrics.current_leader.map(|id| id.to_string()),
                current_term: metrics.current_term,
                last_applied_index: metrics.last_applied.map(|id| id.index),
            };
            if status_tx.send(status).is_err() {
                break;
            }
            if leader_tx.send(role).is_err() {
                break;
            }
        }
    });
}

fn peer_list_to_nodes(peer_ids: &[PeerId]) -> BTreeMap<PeerId, RaftNode> {
    let mut nodes = BTreeMap::new();
    for peer_id in peer_ids {
        nodes.insert(
            *peer_id,
            RaftNode {
                addr: peer_id.to_string(),
            },
        );
        tracing::debug!(peer_id = %peer_id, "configured raft peer id");
    }
    nodes
}
