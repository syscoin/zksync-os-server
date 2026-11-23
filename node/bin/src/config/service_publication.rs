use super::{Config, ConfigValidate, ValidationError, join_validation_path};
use alloy::primitives::{Address, B256};
use smart_config::{DescribeConfig, DeserializeConfig, Serde};
use std::{path::PathBuf, time::Duration};

/// Delegates proof publication to an external service wallet, retaining canonical receipt checks
/// and batch advancement in the node. Commit and execute senders remain node-owned.
#[derive(Clone, Debug, DescribeConfig, DeserializeConfig)]
#[config(derive(Default))]
pub struct ServicePublicationConfig {
    #[config(default)]
    pub enabled: bool,
    #[config(default_t = PathBuf::from("./service-publication"))]
    pub directory: PathBuf,
    #[config(default, with = Serde![str])]
    pub gate: Address,
    #[config(default, with = Serde![str])]
    pub gate_code_hash: B256,
    #[config(default, with = Serde![str])]
    pub coordinator: Address,
    #[config(default, with = Serde![str])]
    pub coordinator_code_hash: B256,
    #[config(default, with = Serde![str])]
    pub policy_hash: B256,
    #[config(default, with = Serde![str])]
    pub production_vk_hash: B256,
    #[config(default, with = Serde![str])]
    pub sequencer: Address,
    #[config(default_t = Duration::from_secs(2))]
    pub poll_interval: Duration,
    #[config(default_t = Duration::from_secs(30))]
    pub rpc_timeout: Duration,
    /// Archive only canonically executed ranges; proved-but-unexecuted work is required on restart.
    #[config(default_t = 256)]
    pub max_records: usize,
}

#[async_trait::async_trait(?Send)]
impl ConfigValidate for ServicePublicationConfig {
    fn validate_conditional(&self, root: &Config, errors: &mut Vec<ValidationError>, prefix: &str) {
        if !self.enabled {
            return;
        }
        for (valid, message) in [
            (
                root.general_config.node_role.is_main() && root.batcher_config.enabled,
                "requires a main node with the batcher enabled",
            ),
            (
                root.prover_api_config.enabled
                    && !root.prover_api_config.fake_fri_provers.enabled
                    && !root.prover_api_config.fake_snark_provers.enabled,
                "requires real FRI and SNARK proving",
            ),
            (self.directory.is_absolute(), "directory must be absolute"),
            (
                !self.gate.is_zero()
                    && !self.gate_code_hash.is_zero()
                    && (self.coordinator.is_zero() == self.coordinator_code_hash.is_zero())
                    && !self.policy_hash.is_zero()
                    && !self.production_vk_hash.is_zero()
                    && !self.sequencer.is_zero(),
                "requires nonzero reviewed contract and release pins",
            ),
            (
                !self.poll_interval.is_zero() && !self.rpc_timeout.is_zero(),
                "poll and RPC timeouts must be positive",
            ),
            (
                self.max_records > 0 && self.max_records <= 100_000,
                "max_records must be in 1..=100000",
            ),
        ] {
            if !valid {
                errors.push(ValidationError::new(
                    join_validation_path(prefix, "enabled"),
                    message,
                ));
            }
        }
    }
}
