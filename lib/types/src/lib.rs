use alloy::primitives::{Address, B256, U256, address, b256, keccak256};

/// SYSCOIN: Consensus target baked into the canonical Syscoin zksync-os guest for compact edge-DA commit
/// transactions. A runtime deployment must match this address before it can collect or verify
/// compact edge references.
pub const SYSCOIN_COMPACT_EDGE_DA_COMMIT_TARGET: Address =
    address!("0xd0ec30807902886b61a86d9bd209fe353c1d912b");

/// SYSCOIN: Exact deployed EVM runtime identity of
/// [`SYSCOIN_COMPACT_EDGE_DA_COMMIT_TARGET`]. Both length and hash are attested so a partial or
/// corrupted Gateway snapshot cannot authorize an empty or incompatible ValidatorTimelock proxy
/// shell. The upgradeable implementation remains governed by the canonical on-chain proxy state.
pub const SYSCOIN_COMPACT_EDGE_DA_COMMIT_TARGET_RUNTIME_SIZE: u32 = 2_840;
pub const SYSCOIN_COMPACT_EDGE_DA_COMMIT_TARGET_RUNTIME_HASH: B256 =
    b256!("ed00d115b16594117ebb53b6d0322ada70270ee75e2b7e8eed5e33967c3fb777");

/// SYSCOIN: Canonical zkSYS fee-tank address baked into the V8 guest.
pub const SYSCOIN_GAS_TANK_ADDRESS: Address =
    address!("0xb49943ea232624dd4aa63e18186076c6c99a68ef");

/// SYSCOIN: Exact deployed EVM runtime identity of [`SYSCOIN_GAS_TANK_ADDRESS`].
/// The constructor-specialized runtime also binds the immutable canonical zkSYS token address.
pub const SYSCOIN_GAS_TANK_RUNTIME_HASH: B256 =
    b256!("041faf31b2f3576502f25fd5d106eaf411611e42dc996c28872abe487cb6e269");

/// SYSCOIN: The only Gateway chain whose guest may collect forwarded compact Edge-DA refs.
pub const SYSCOIN_GATEWAY_CHAIN_ID: u64 = 57_001;

/// SYSCOIN: Canonical relay deployed through Arachnid's universal CREATE2 factory. The guest
/// authenticates this address as the `L1Messenger` message origin; it is not configurable.
pub const SYSCOIN_COMPACT_EDGE_DA_RELAY_EMITTER: Address =
    address!("0x758b06cda80bdd016f79afd0df1a984039067a21");

/// SYSCOIN: Exact frozen runtime identity of [`SYSCOIN_COMPACT_EDGE_DA_RELAY_EMITTER`].
pub const SYSCOIN_COMPACT_EDGE_DA_RELAY_RUNTIME_HASH: B256 =
    b256!("4c86ffe57098cb09a48ee6dfa4f21b2cce8e327409e1da1dc6be4545220b89e0");

/// SYSCOIN: Arachnid deterministic-deployment proxy and its exact canonical runtime identity.
pub const SYSCOIN_EDGE_DA_RELAY_FACTORY: Address =
    address!("0x4e59b44847b379578588920ca78fbf26c0b4956c");
pub const SYSCOIN_EDGE_DA_RELAY_FACTORY_RUNTIME_HASH: B256 =
    b256!("2fa86add0aed31f33a762c9d88e807c475bd51d0f52bd0955754b2608f7e4989");

/// SYSCOIN: Canonical EIP-7825 transaction gas cap committed by the V8 chain configuration.
///
/// This value is consensus-critical: the native PIG, L1 proof calldata, and Era executor must all
/// hash the same value.
pub const SYSCOIN_MAX_TX_GAS_LIMIT: u64 = 1 << 24;

/// SYSCOIN: Canonical V8 chain configuration commitment.
///
/// The L2 chain ID is committed here instead of being repeated in `BatchOutput`. The middle word
/// is `fri_proof_verification_enabled`, which is permanently disabled for this deployment.
pub fn syscoin_chain_config_hash(chain_id: u64) -> B256 {
    let mut bytes = Vec::with_capacity(32 * 3);
    bytes.extend_from_slice(&U256::from(chain_id).to_be_bytes::<32>());
    bytes.extend_from_slice(&U256::ZERO.to_be_bytes::<32>());
    bytes.extend_from_slice(&U256::from(SYSCOIN_MAX_TX_GAS_LIMIT).to_be_bytes::<32>());
    keccak256(bytes)
}

mod config_format;
pub use config_format::ConfigFormat;

mod transaction_acceptance_state;
pub use transaction_acceptance_state::{
    BackpressureCause, BackpressureTrigger, NotAcceptingReason, TransactionAcceptanceState,
};

mod block;
pub use block::BlockExt;

mod log;
pub use log::{L2_TO_L1_TREE_SIZE, L2ToL1Log};

mod receipt;
pub use receipt::{ZkReceipt, ZkReceiptEnvelope};

mod transaction;
pub use transaction::{
    Eip2718, IndexedInteropRoot, InteropRootsLogIndex, L1_TX_MINIMAL_GAS_LIMIT, L1Envelope,
    L1EnvelopeError, L1PriorityEnvelope, L1PriorityTx, L1PriorityTxType, L1Tx, L1TxSerialId,
    L1TxType, L1UpgradeEnvelope, L1UpgradeTx, L2_INTEROP_COMMITMENT_TREE_ADDRESS,
    L2_INTEROP_ROOT_STORAGE_ADDRESS, L2Envelope, L2Transaction,
    REQUIRED_L1_TO_L2_GAS_PER_PUBDATA_BYTE, SYSTEM_CONTEXT_ADDRESS, SYSTEM_TX_TYPE_ID,
    SystemTxEnvelope, SystemTxType, TransactionData, UpgradeInfo, UpgradeMetadata, UpgradeTxType,
    ZkEnvelope, ZkTransaction, ZkTxType, ZksyncOsEncode,
    utils::{BOOTLOADER_FORMAL_ADDRESS, L2_INTEROP_CENTER_ADDRESS},
};

pub use zksync_os_contract_interface::InteropRoot;

mod pubdata_mode;
pub use pubdata_mode::PubdataMode;

mod node;
pub use node::NodeRole;

mod protocol;
pub use protocol::{
    ExecutionVersion, ExecutionVersionError, ProtocolSemanticVersion, ProtocolSemanticVersionError,
    ProvingVersion, ProvingVersionError,
};

mod block_start_cursors;
pub use block_start_cursors::BlockStartCursors;

mod token_price;
pub use token_price::{TokenApiRatio, TokenPricesForFees};

mod block_output;
pub use block_output::{BlockOutput, BlockPubdata};

mod fee_params;
pub use fee_params::FeeParams;
