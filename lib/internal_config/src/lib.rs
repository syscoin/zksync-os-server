//! Module for managing internal configuration, i.e. config that the node can set for itself.
//! Internal config is stored in a JSON file on disk and read/written as needed.
//! Internal config is expected to be read at node startup and merged with the main config.

use alloy::primitives::Address;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Structure of the internal configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InternalConfig {
    /// Number of the failing block that node wants to empty (causing a reorg).
    pub failing_block: Option<u64>,
    /// List of L2 signer addresses to blacklist (i.e. their transactions are rejected).
    /// To be merged with the external blacklist in the main config.
    #[serde(default)]
    pub l2_signer_blacklist: HashSet<Address>,
    // SYSCOIN: signers newly added to `l2_signer_blacklist` as temporary REVM
    // divergence mitigation, so cleanup can remove only those entries.
    #[serde(default)]
    pub revm_divergence_l2_signer_blacklist: HashSet<Address>,
}

/// Manager for reading and writing the internal configuration file.
/// Each write operation panics the node to ensure it restarts with the updated config.
#[derive(Debug, Clone)]
pub struct InternalConfigManager {
    /// Path to the internal configuration file.
    pub file_path: PathBuf,
    /// Lock to ensure exclusive access to the config file during writes.
    pub file_lock: Arc<Mutex<()>>,
}

impl InternalConfig {
    // SYSCOIN: record only signers newly blacklisted by REVM divergence handling.
    pub fn insert_revm_divergence_l2_signer(&mut self, signer: Address) -> bool {
        if self.l2_signer_blacklist.insert(signer) {
            self.revm_divergence_l2_signer_blacklist.insert(signer);
            true
        } else {
            false
        }
    }

    // SYSCOIN: clear temporary REVM divergence state without dropping pre-existing
    // blacklist entries.
    pub fn clear_revm_divergence_mitigation(&mut self) {
        self.failing_block = None;
        for signer in self.revm_divergence_l2_signer_blacklist.drain() {
            self.l2_signer_blacklist.remove(&signer);
        }
    }
}

impl InternalConfigManager {
    pub fn new(file_path: PathBuf) -> anyhow::Result<Self> {
        if !file_path.exists() {
            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent)
                    .context("Failed to create directories for internal config file")?;
            }
            std::fs::write(&file_path, "{}").context("Failed to create internal config file")?;
            tracing::info!(
                "Created new internal config file at {}",
                file_path.display()
            );
        }
        Ok(Self {
            file_path,
            file_lock: Arc::new(Mutex::new(())),
        })
    }

    pub fn read_config(&self) -> anyhow::Result<InternalConfig> {
        let file =
            std::fs::File::open(&self.file_path).context("Failed to open internal config file")?;
        serde_json::from_reader(file).context("Failed to parse internal config file")
    }

    pub fn write_config_and_panic(
        &self,
        config: &InternalConfig,
        panic_message: &str,
    ) -> anyhow::Result<()> {
        // Acquire the lock to ensure exclusive access to the file.
        let _lock = self
            .file_lock
            .lock()
            .map_err(|err| anyhow::anyhow!("failed to acquire file lock: {err}"))?;

        let file = std::fs::File::create(&self.file_path)
            .context("Failed to create internal config file for writing")?;
        serde_json::to_writer_pretty(file, config)
            .context("Failed to write internal config to file")?;

        panic!("Internal config was updated, panicking: {panic_message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_revm_divergence_mitigation_preserves_pre_existing_blacklist_entries() {
        let pre_existing = Address::repeat_byte(0x01);
        let temporary = Address::repeat_byte(0x02);

        let mut config = InternalConfig {
            failing_block: Some(42),
            l2_signer_blacklist: HashSet::from([pre_existing, temporary]),
            revm_divergence_l2_signer_blacklist: HashSet::from([temporary]),
        };

        config.clear_revm_divergence_mitigation();

        assert_eq!(config.failing_block, None);
        assert!(config.l2_signer_blacklist.contains(&pre_existing));
        assert!(!config.l2_signer_blacklist.contains(&temporary));
        assert!(config.revm_divergence_l2_signer_blacklist.is_empty());
    }

    #[test]
    fn insert_revm_divergence_l2_signer_tracks_only_new_blacklist_entries() {
        let pre_existing = Address::repeat_byte(0x01);
        let temporary = Address::repeat_byte(0x02);

        let mut config = InternalConfig {
            failing_block: Some(42),
            l2_signer_blacklist: HashSet::from([pre_existing]),
            revm_divergence_l2_signer_blacklist: HashSet::new(),
        };

        assert!(!config.insert_revm_divergence_l2_signer(pre_existing));
        assert!(config.insert_revm_divergence_l2_signer(temporary));

        assert!(config.l2_signer_blacklist.contains(&pre_existing));
        assert!(config.l2_signer_blacklist.contains(&temporary));
        assert!(
            !config
                .revm_divergence_l2_signer_blacklist
                .contains(&pre_existing)
        );
        assert!(
            config
                .revm_divergence_l2_signer_blacklist
                .contains(&temporary)
        );
    }

    #[test]
    fn internal_config_deserializes_without_revm_divergence_blacklist() {
        let config: InternalConfig = serde_json::from_str(
            r#"{
                "failing_block": 42,
                "l2_signer_blacklist": []
            }"#,
        )
        .expect("legacy internal config should deserialize");

        assert_eq!(config.failing_block, Some(42));
        assert!(config.l2_signer_blacklist.is_empty());
        assert!(config.revm_divergence_l2_signer_blacklist.is_empty());
    }
}
