use std::ops::RangeInclusive;
use zksync_os_contract_interface::l1_discovery::L1State;
use zksync_os_types::NodeRole;

#[allow(dead_code)] // some fields are only used for logging (`Debug`)
#[derive(Debug, Clone)]
pub struct NodeStateOnStartup {
    pub node_role: NodeRole,
    pub l1_state: L1State,
    pub state_block_range_available: RangeInclusive<u64>,
    pub block_replay_storage_last_block: u64,
    pub tree_last_block: u64,
    pub repositories_persisted_block: u64,
    pub last_l1_committed_block: u64,
    pub last_l1_proved_block: u64,
    pub last_l1_executed_block: u64,
}

impl NodeStateOnStartup {
    pub fn assert_consistency(&self, archive_verification_anchor: Option<u64>) {
        assert!(
            self.last_l1_committed_block >= self.last_l1_proved_block,
            "Last committed block ({}) is less than last proved block ({})",
            self.last_l1_committed_block,
            self.last_l1_proved_block,
        );
        assert!(
            self.last_l1_proved_block >= self.last_l1_executed_block,
            "Last proved block ({}) is less than last executed block ({})",
            self.last_l1_proved_block,
            self.last_l1_executed_block,
        );
        validate_replay_wal_head(
            self.node_role,
            self.block_replay_storage_last_block,
            self.last_l1_committed_block,
            archive_verification_anchor,
        )
        .expect("inconsistent replay WAL head at startup");
    }
}

fn validate_replay_wal_head(
    role: NodeRole,
    wal_head: u64,
    last_l1_committed_block: u64,
    archive_verification_anchor: Option<u64>,
) -> anyhow::Result<()> {
    if let Some(anchor) = archive_verification_anchor {
        // An isolated verifier replays to the operator's trusted backup. Settlement may have
        // advanced beyond it while the backup was copied. Normal main-node startup must still
        // cover the live committed frontier before it can produce or serve peers.
        anyhow::ensure!(
            wal_head == anchor,
            "archive replay WAL head {wal_head} differs from its trusted anchor {anchor}"
        );
    } else if role.is_main() {
        anyhow::ensure!(
            wal_head >= last_l1_committed_block,
            "Block replay WAL head ({wal_head}) is behind the last L1-committed block ({last_l1_committed_block}); \
             check DB consistency and restore the WAL from a replay archive if available"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{NodeRole, validate_replay_wal_head};

    #[test]
    fn older_external_node_backup_can_be_verified_without_lowering_the_main_node_frontier() {
        let main = NodeRole::MainNode;
        assert!(validate_replay_wal_head(main, 80, 100, Some(80)).is_ok());
        assert!(validate_replay_wal_head(main, 79, 100, Some(80)).is_err());
        assert!(validate_replay_wal_head(main, 81, 100, Some(80)).is_err());
        assert!(validate_replay_wal_head(NodeRole::ExternalNode, 80, 100, None).is_ok());
        assert!(validate_replay_wal_head(main, 80, 100, None).is_err());
        assert!(validate_replay_wal_head(main, 100, 100, None).is_ok());
    }
}
