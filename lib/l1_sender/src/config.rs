use std::marker::PhantomData;
use std::time::Duration;
use zksync_os_operator_signer::SignerConfig;

/// Configuration of L1 sender.
#[derive(Clone, Debug)]
pub struct L1SenderConfig<Input> {
    /// Operator signer configuration.
    /// Depending on the mode, this can be a commit/prove/execute operator.
    /// Supports both local private keys and GCP KMS keys.
    pub operator_signer: SignerConfig,

    /// Fee caps and replacement multipliers for L1 transactions.
    pub fee_config: L1SenderFeeConfig,

    /// Whether to skip in-flight recovery and replace pending L1 transactions on startup.
    pub force_transaction_resubmission: bool,

    /// Max number of commands (to commit/prove/execute one batch) to be processed at a time.
    pub command_limit: usize,

    /// How often to poll L1 for new blocks.
    pub poll_interval: Duration,

    /// SYSCOIN: warning interval while waiting for an L1 transaction to be included.
    ///
    /// A delayed L1 transaction must not terminate the main node; the sender keeps waiting and
    /// logs every time this interval elapses.
    pub transaction_timeout: Duration,

    /// SYSCOIN: how often to poll the settlement-layer mempool while waiting for a receipt.
    ///
    /// This detects transactions accepted by the RPC and later dropped or permanently rejected
    /// before the longer warning interval elapses.
    pub tx_liveness_poll_interval: Duration,

    /// SYSCOIN: consecutive missing mempool polls required before treating a tx as dropped.
    ///
    /// A value of `0` disables the dropped-tx liveness check.
    pub tx_liveness_max_missing_polls: u32,

    /// Use Fusaka blob transaction format if the timestamp has passed.
    pub fusaka_upgrade_timestamp: u64,

    pub phantom_data: PhantomData<Input>,
}

/// Fee configuration for L1 sender transactions.
#[derive(Clone, Copy, Debug)]
pub struct L1SenderFeeConfig {
    /// Max fee per gas we are willing to spend (in wei).
    pub max_fee_per_gas_wei: u128,

    /// Max priority fee per gas we are willing to spend (in wei).
    pub max_priority_fee_per_gas_wei: u128,

    /// Max fee per blob gas we are willing to spend (in wei).
    pub max_fee_per_blob_gas_wei: u128,

    /// Multiplier applied to `max_fee_per_gas_wei` when forcing transaction resubmission.
    pub max_fee_per_gas_replacement_multiplier: f64,

    /// Multiplier applied to `max_priority_fee_per_gas_wei` when forcing transaction resubmission.
    pub max_priority_fee_per_gas_replacement_multiplier: f64,

    /// Multiplier applied to `max_fee_per_blob_gas_wei` when forcing transaction resubmission.
    pub max_fee_per_blob_gas_replacement_multiplier: f64,
}
