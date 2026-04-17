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

    /// Max fee per gas we are willing to spend (in wei).
    pub max_fee_per_gas_wei: u128,

    /// Max priority fee per gas we are willing to spend (in wei).
    pub max_priority_fee_per_gas_wei: u128,

    /// Max fee per blob gas we are willing to spend (in wei).
    pub max_fee_per_blob_gas_wei: u128,

    /// Max number of commands (to commit/prove/execute one batch) to be processed at a time.
    pub command_limit: usize,

    /// How often to poll L1 for new blocks.
    pub poll_interval: Duration,

    /// Maximum time to wait for a transaction to be included on L1.
    pub transaction_timeout: Duration,

    /// SYSCOIN How often to poll the settlement-layer mempool for an in-flight
    /// transaction while waiting for its receipt. Used to detect permanently
    /// rejected transactions (e.g. dropped by a ZKsync OS gateway with
    /// `source_marked_invalid=true`) instead of waiting the full
    /// `transaction_timeout`.
    pub tx_liveness_poll_interval: Duration,

    /// Number of consecutive polls that must report the transaction as missing
    /// from the settlement-layer mempool (and not yet mined) before the L1
    /// sender declares it permanently rejected and fails. A value of `0`
    /// disables the liveness check entirely (legacy behavior: wait up to
    /// `transaction_timeout`).
    pub tx_liveness_max_missing_polls: u32,

    /// Use Fusaka blob transaction format if the timestamp has passed.
    pub fusaka_upgrade_timestamp: u64,

    pub phantom_data: PhantomData<Input>,
}
