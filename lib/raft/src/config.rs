use std::path::PathBuf;
use std::time::Duration;
use reth_network_peers::PeerId;

#[derive(Clone, Debug)]
pub struct RaftConsensusConfig {
    pub node_id: PeerId,
    pub peer_ids: Vec<PeerId>,
    pub bootstrap: bool,
    pub election_timeout_min: Duration,
    pub election_timeout_max: Duration,
    pub heartbeat_interval: Duration,
    pub storage_path: PathBuf,
}
