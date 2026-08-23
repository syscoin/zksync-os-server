use alloy::primitives::{Address, B256, U256, address, keccak256};

/// SYSCOIN: Consensus target baked into the canonical Syscoin zksync-os guest for compact edge-DA commit
/// transactions. A runtime deployment must match this address before it can collect or verify
/// compact edge references.
pub const SYSCOIN_COMPACT_EDGE_DA_COMMIT_TARGET: Address =
    address!("0x64ef2f0c4168eb76fe95993f2a7c7b35dcf3fe19");

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
