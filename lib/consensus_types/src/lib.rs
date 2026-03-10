use openraft::{BasicNode, Entry, EntryPayload};
use reth_network_peers::PeerId;
use std::io::Cursor;
use zksync_os_storage_api::ReplayRecord;

pub type RaftNode = BasicNode;

openraft::declare_raft_types!(
    pub RaftTypeConfig:
        D = ReplayRecord,
        R = (),
        NodeId = PeerId,
        Node = RaftNode,
);

pub fn display_raft_entry(entry: &Entry<RaftTypeConfig>) -> String {
    let payload = match &entry.payload {
        EntryPayload::Blank => "blank".to_string(),
        EntryPayload::Normal(r) => format!(
            "block number {} (block output hash: {})",
            r.block_context.block_number, r.block_output_hash
        ),
        EntryPayload::Membership(_) => "membership".to_string(),
    };
    format!("Entry(log_id_index={}, {})", entry.log_id.index, payload)
}
