use crate::limits::Limits;
use alloy::primitives::{Address, B256};
use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;
use std::time::Duration;

/// Rate-limit configuration.
#[derive(Clone, Debug, Default)]
pub enum RateLimits {
    /// No rate limiting.
    #[default]
    None,
    /// One global cap, plus a shared M-method bucket, plus per-method custom overrides.
    ///
    /// Production example:
    ///
    /// ```yaml
    /// rpc:
    ///   rate_limits:
    ///     type: Tiered
    ///     global_rps: 1000
    ///     m_rps: 200
    ///     m_methods:
    ///       - eth_call
    ///       - eth_estimateGas
    ///       - eth_getBlockReceipts
    ///       - eth_fillTransaction
    ///       - zks_getProof
    ///       - ots_getBlockTransactions
    ///       - txpool_inspect
    ///     custom_methods:
    ///       eth_getLogs: 200
    ///       eth_simulateV1: 1
    ///       debug_traceTransaction: 10
    ///       debug_traceCall: 10
    ///       debug_traceBlockByHash: 10
    ///       debug_traceBlockByNumber: 10
    ///       zks_getL2ToL1LogProof: 10
    ///       ots_searchTransactionsBefore: 10
    ///       ots_searchTransactionsAfter: 10
    ///       txpool_content: 10
    /// ```
    Tiered {
        global_rps: NonZeroU32,
        m_rps: NonZeroU32,
        m_methods: HashSet<String>,
        custom_methods: HashMap<String, NonZeroU32>,
    },
}

impl RateLimits {
    pub(crate) fn into_limits(self) -> Limits {
        match self {
            Self::None => Limits::default(),
            Self::Tiered {
                global_rps,
                m_rps,
                m_methods,
                custom_methods,
            } => Limits {
                global_rps: Some(global_rps),
                // SYSCOIN: keep `m_methods` grouped so `m_rps` is enforced as a shared bucket.
                m_rps: Some(m_rps),
                m_methods,
                methods: custom_methods,
            },
        }
    }
}

#[derive(Clone, Debug)]
pub struct EdgeDaAdmissionConfig {
    pub commit_tx_target: Address,
    pub rpc_url: String,
    pub rpc_user: String,
    pub rpc_password: String,
    pub poda_url: String,
    pub wallet_name: String,
    pub request_timeout: Duration,
}

#[derive(Clone, Debug)]
pub struct RpcConfig {
    /// JSON-RPC address to listen on. Only http is currently supported.
    pub address: String,

    /// Gas limit of transactions executed via eth_call
    pub eth_call_gas: usize,

    /// Maximum execution time of a single JS tracer run
    pub js_tracer_timeout: Duration,

    /// Maximum memory growth (in bytes) allowed during a single JS tracer run, measured via
    /// jemalloc per-thread allocation counters; `0` disables the check
    pub js_tracer_max_memory_bytes: usize,

    /// Maximum block gas limit accepted for an `eth_simulateV1` block override. Applies only
    /// when the caller explicitly overrides `blockOverrides.gasLimit`; unset overrides fall
    /// back to the executing block's own gas limit.
    pub eth_simulate_block_gas_limit: u64,

    /// Number of concurrent API connections (passed to jsonrpsee, default value there is 128)
    pub max_connections: u32,

    // SYSCOIN: cap resource-heavy blocking JSON-RPC handlers independently from
    // the connection limit so public endpoints cannot fan out unlimited VM/state work.
    pub max_concurrent_blocking_rpcs: u32,

    /// Maximum number of active subscriptions accepted per websocket connection.
    pub max_subscriptions_per_connection: u32,

    /// Maximum RPC request payload size for both HTTP and WS in megabytes
    pub max_request_size: u32,

    /// Maximum RPC response payload size for both HTTP and WS in megabytes
    pub max_response_size: u32,

    /// Maximum number of blocks that could be scanned per filter
    pub max_blocks_per_filter: u64,

    /// Maximum number of logs that can be returned in a response
    pub max_logs_per_response: usize,

    /// Duration since the last filter poll, after which the filter is considered stale
    pub stale_filter_ttl: Duration,

    /// List of L2 signer addresses to blacklist (i.e. their transactions are rejected).
    pub l2_signer_blacklist: HashSet<Address>,

    // SYSCOIN: exact L2 transaction hashes to reject, used for known
    // REVM-divergent transactions without permanently blacklisting signers.
    pub l2_tx_blacklist: HashSet<B256>,

    /// Default timeout for `eth_sendRawTransactionSync`
    pub send_raw_transaction_sync_timeout: Duration,

    /// Factor applied to the pending block base fee returned by `eth_gasPrice`.
    pub gas_price_scale_factor: f64,

    /// Factor for pubdata price used during gas limit estimation (`eth_estimateGas`).
    /// Needed to account for pubdata price market fluctuations. Setting this to `1.0` can lead to
    /// users submitting unexecutable transactions (fail with `OutOfNativeResourcesDuringValidation`)
    /// because pubdata price increase in-between estimation and sequencing.
    pub estimate_gas_pubdata_price_factor: f64,

    // SYSCOIN: keep resource-intensive debug RPC methods off unless an operator opts in.
    pub enable_debug_namespace: bool,

    // SYSCOIN: keep mempool-enumerating txpool RPC methods off on public endpoints
    // unless an operator explicitly exposes them on a trusted interface.
    pub enable_txpool_namespace: bool,

    // SYSCOIN: when this node is a Gateway sequencer, reject compact edge DA commit txs
    // before mempool admission unless every referenced Bitcoin DA blob is retrievable.
    pub edge_da_admission: Option<EdgeDaAdmissionConfig>,

    /// Rate limits for incoming requests.
    pub rate_limits: RateLimits,

    /// List of disabled methods.
    /// Some stateful methods like `eth_newFilter` don't make sense when running in a cluster behind a load-balancer.
    /// They get rejected with -32601 "Method disabled".
    pub method_filter: HashSet<String>,
}

impl RpcConfig {
    /// Returns the max request size in bytes.
    pub fn max_request_size_bytes(&self) -> u32 {
        self.max_request_size.saturating_mul(1024 * 1024)
    }

    /// Returns the max response size in bytes.
    pub fn max_response_size_bytes(&self) -> u32 {
        self.max_response_size.saturating_mul(1024 * 1024)
    }
}
