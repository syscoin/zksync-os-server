use crate::{BatchSignatureSet, PendingBatchInfo};
use alloy::primitives::{Address, B256, Bytes, address, keccak256};
use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fmt::{Debug, Formatter};
use std::time::SystemTime;
use time::UtcDateTime;
use zksync_os_batcher_metrics::{BATCHER_METRICS, BatchExecutionStage};
use zksync_os_contract_interface::models::{L2Log, StoredBatchInfo};
use zksync_os_observability::LatencyDistributionTracker;
use zksync_os_pipeline::HasBlockRangeEnd;
use zksync_os_types::{BlockOutput, L2_INTEROP_CENTER_ADDRESS, ProvingVersion, PubdataMode};

// SYSCOIN: V32 InteropCenter bundles are emitted through the canonical L2-to-L1 messenger.
pub const L2_TO_L1_MESSENGER_ADDRESS: Address =
    address!("0x0000000000000000000000000000000000008008");
// SYSCOIN: Pinned Era V32 `Messaging.sol` prefixes InteropCenter bundles with this byte.
pub const INTEROP_BUNDLE_IDENTIFIER: u8 = 0x01;

/// SYSCOIN: Authenticates one canonical InteropCenter bundle log against its retained message
/// preimage. Checking the messenger, caller key, and preimage hash prevents arbitrary user messages
/// from opting into the priority proving lane by copying the bundle prefix.
pub fn is_interop_bundle_log(
    l2_shard_id: u8,
    is_service: bool,
    sender: Address,
    key: B256,
    value: B256,
    message: &[u8],
) -> bool {
    l2_shard_id == 0
        && is_service
        && sender == L2_TO_L1_MESSENGER_ADDRESS
        && key == B256::left_padding_from(L2_INTEROP_CENTER_ADDRESS.as_slice())
        && message.first().copied() == Some(INTEROP_BUNDLE_IDENTIFIER)
        && keccak256(message) == value
}

/// SYSCOIN: Detects an authenticated InteropCenter bundle directly in canonical block output.
/// Only successful transactions count: reverted calls may retain diagnostic logs in VM output,
/// but cannot create a durable interop signal or arm the companion-batch policy.
pub fn block_contains_interop_bundle(block: &BlockOutput) -> bool {
    block.tx_results.iter().flatten().any(|tx_output| {
        tx_output.is_success()
            && tx_output.l2_to_l1_logs.iter().any(|log| {
                log.preimage.as_deref().is_some_and(|message| {
                    is_interop_bundle_log(
                        log.log.l2_shard_id,
                        log.log.is_service,
                        log.log.sender,
                        log.log.key,
                        log.log.value,
                        message,
                    )
                })
            })
    })
}

/// Information about a batch that is enough for all L1 operations.
/// Used throughout the batcher subsystem
/// We may want to rework it -
///    instead of putting computed CommitBatchInfo/StoredBatchInfo here (L1 contract-specific classes),
///    we may want to include lower-level fields
///
///  Note that we serialize it in `ProofStorage`, so a change here will invalidate old entries
///  This isn't really a problem as we only store the recent ones
///
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BatchMetadata {
    pub previous_stored_batch_info: StoredBatchInfo,
    // This is not purely commitment information, but we keep old serialization name for
    // backwards-compatibility.
    #[serde(rename = "commit_batch_info")]
    pub batch_info: PendingBatchInfo,
    pub chain_address: Address,
    pub first_block_number: u64,
    pub last_block_number: u64,
    pub last_block_hash: Option<B256>,
    pub pubdata_mode: PubdataMode,
    // note: can equal to zero
    pub tx_count: usize,
    #[serde(default)]
    pub computational_native_used: Option<u64>,
    #[serde(default)]
    pub logs: Vec<L2Log>,
    #[serde(default)]
    pub messages: Vec<Vec<u8>>,
    #[serde(default)]
    pub multichain_root: B256,
    /// Migration number of the `SetSLChainId` system transaction executed in this batch, if any.
    /// `None` for the vast majority of batches; `Some(n)` only for the single batch that contains
    /// the `SetSLChainId` transaction triggered by a gateway migration.
    #[serde(default)]
    pub set_sl_chain_id_migration_number: Option<u64>,
}

impl BatchMetadata {
    /// Gets batch metadata verification key hash.
    pub fn verification_key_hash(&self) -> anyhow::Result<&'static str> {
        Ok(
            ProvingVersion::try_from(self.batch_info.protocol_version.clone())
                .context("Failed to get proving version from protocol version")?
                .vk_hash(),
        )
    }

    pub fn proving_version(&self) -> anyhow::Result<ProvingVersion> {
        Ok(ProvingVersion::try_from(
            self.batch_info.protocol_version.clone(),
        )?)
    }

    /// SYSCOIN: Returns whether this durable batch contains an authenticated V32 InteropCenter
    /// bundle. The same predicate drives batch isolation and priority SNARK readiness.
    pub fn contains_interop_bundle(&self) -> bool {
        self.logs.iter().any(|log| {
            self.messages.iter().any(|message| {
                is_interop_bundle_log(
                    log.l2_shard_id,
                    log.is_service,
                    log.sender,
                    log.key,
                    log.value,
                    message,
                )
            })
        })
    }
}

#[derive(Debug)]
pub struct MissingSignature;

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub enum BatchSignatureData {
    Signed {
        signatures: BatchSignatureSet,
    },
    /// Batch was already committed, but is going through pipeline the second time.
    /// We do not need to have signatures for it now
    AlreadyCommitted,
    // default to allow deserializing of older objects
    /// Batch signatures are not enabled
    #[default]
    NotNeeded,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BatchEnvelope<E, S> {
    pub batch: BatchMetadata,
    pub data: E,
    #[serde(default)] // to allow deserializing older objects
    pub signature_data: S,
    #[serde(skip, default)]
    pub latency_tracker: LatencyDistributionTracker<BatchExecutionStage>,
}

pub type BatchForSigning<E> = BatchEnvelope<E, MissingSignature>;
pub type SignedBatchEnvelope<E> = BatchEnvelope<E, BatchSignatureData>;

impl<E> BatchEnvelope<E, MissingSignature> {
    pub fn new(batch: BatchMetadata, data: E) -> Self {
        Self {
            batch,
            data,
            signature_data: MissingSignature,
            latency_tracker: LatencyDistributionTracker::default(),
        }
    }

    pub fn with_signatures(
        self,
        signature_data: BatchSignatureData,
    ) -> BatchEnvelope<E, BatchSignatureData> {
        BatchEnvelope {
            batch: self.batch,
            data: self.data,
            signature_data,
            latency_tracker: self.latency_tracker,
        }
    }
}

impl<E, S> BatchEnvelope<E, S> {
    pub fn batch_number(&self) -> u64 {
        self.batch.batch_info.batch_number
    }
    pub fn time_since_first_block(&self) -> anyhow::Result<core::time::Duration> {
        let first_block_time = SystemTime::from(UtcDateTime::from_unix_timestamp(
            self.batch.batch_info.first_block_timestamp as i64,
        )?);

        Ok(SystemTime::now().duration_since(first_block_time)?)
    }

    // not 100% happy with this - `BatchEnvelope` shouldn't depend on metrics
    // maybe we can put metrics logic inside `LatencyDistributionTracker` generically,
    // but then it needs to have the batch_number as its field - which makes it non-generic.
    // On the other hand, we can treat the `BatchEnvelop` model as metrics/tracking-related
    //
    // Will be revisited on next `BatchEnvelope` iteration -
    // along with the fact that we almost always only use `BatchEnvelope<FriProof>`, so it being generic may be not justified

    pub fn set_stage(&mut self, stage: BatchExecutionStage) {
        let batch_number = self.batch_number();
        let last_block_number = self.batch.last_block_number;
        self.latency_tracker.record_stage(stage, |duration| {
            BATCHER_METRICS.execution_stages[&stage].observe(duration);
            if !matches!(
                stage,
                BatchExecutionStage::CommitL1Passthrough
                    | BatchExecutionStage::ProveL1Passthrough
                    | BatchExecutionStage::ExecuteL1Passthrough
            ) {
                BATCHER_METRICS.batch_number[&stage].set(batch_number);
                BATCHER_METRICS.block_number[&stage].set(last_block_number);
            }
        });
    }

    pub fn with_stage(mut self, stage: BatchExecutionStage) -> BatchEnvelope<E, S> {
        self.set_stage(stage);
        self
    }

    pub fn with_data<N>(self, data: N) -> BatchEnvelope<N, S> {
        BatchEnvelope {
            batch: self.batch,
            data,
            signature_data: self.signature_data,
            latency_tracker: self.latency_tracker,
        }
    }
}

/// Input data required to generate a ZK proof for a batch.
///
/// Used for tests and testnets where the expensive RiscV witness computation is unnecessary.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ProverInput {
    Real(Vec<u32>),
    Fake,
}

impl ProverInput {
    /// Returns the underlying witness words.
    /// Panics if called on `Fake`.
    pub fn unwrap_real(&self) -> &[u32] {
        match self {
            ProverInput::Real(v) => v.as_slice(),
            ProverInput::Fake => panic!("ProverInput::Fake has no witness data"),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub enum FriProof {
    // Fake proof for testing purposes
    Fake,
    // Marker for batches that were already proven on L1, so we don't need to prove them again
    AlreadySubmittedToL1,
    Real(RealFriProof),
}

#[derive(Clone, Serialize, Deserialize)]
/// SYSCOIN: Fresh V32 storage has one canonical, version-stamped FRI proof envelope.
pub struct RealFriProof {
    pub proof: Bytes,
    pub proving_execution_version: u32,
}

impl FriProof {
    pub fn is_fake(&self) -> bool {
        matches!(self, FriProof::Fake)
    }

    pub fn proving_execution_version(&self) -> Option<u32> {
        match self {
            FriProof::Real(proof) => Some(proof.proving_execution_version),
            _ => None,
        }
    }

    pub fn proof(&self) -> Option<&[u8]> {
        match self {
            FriProof::Real(real) => Some(real.proof()),
            FriProof::Fake | FriProof::AlreadySubmittedToL1 => None,
        }
    }
}

impl RealFriProof {
    pub fn proof(&self) -> &[u8] {
        self.proof.as_ref()
    }
}

impl Debug for FriProof {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            FriProof::Fake => write!(f, "Fake"),
            FriProof::AlreadySubmittedToL1 => write!(f, "AlreadySubmittedToL1"),
            FriProof::Real(_) => write!(
                f,
                "Real(proving_execution_version={:?}, len: {:?})",
                self.proving_execution_version(),
                self.proof().unwrap().len()
            ),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SnarkProof {
    // Fake proof for testing purposes
    Fake,
    Real(RealSnarkProof),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// SYSCOIN: Fresh V32 storage has one canonical, version-stamped SNARK proof envelope.
pub struct RealSnarkProof {
    pub proof: Vec<u8>,
    pub proving_execution_version: u32,
}

impl SnarkProof {
    pub fn proving_execution_version(&self) -> Option<u32> {
        match self {
            SnarkProof::Real(proof) => Some(proof.proving_execution_version),
            _ => None,
        }
    }

    pub fn proof(&self) -> Option<&[u8]> {
        match self {
            SnarkProof::Real(real) => Some(real.proof()),
            SnarkProof::Fake => None,
        }
    }
}

impl RealSnarkProof {
    pub fn proof(&self) -> &[u8] {
        self.proof.as_slice()
    }
}

impl<E: Send + 'static, S: Send + 'static> HasBlockRangeEnd for BatchEnvelope<E, S> {
    fn block_number(&self) -> u64 {
        self.batch.last_block_number
    }
    fn block_timestamp(&self) -> Option<u64> {
        Some(self.batch.batch_info.last_block_timestamp)
    }
    fn batch_number(&self) -> Option<u64> {
        Some(self.batch.batch_info.batch_number)
    }
}

#[cfg(test)]
mod tests {
    use super::{L2_TO_L1_MESSENGER_ADDRESS, block_contains_interop_bundle, is_interop_bundle_log};
    use alloy::consensus::{Header, Sealable};
    use alloy::primitives::{Address, B256, keccak256};
    use zksync_os_interface::types::{
        ExecutionOutput, ExecutionResult, L2ToL1Log, L2ToL1LogWithPreimage, TxOutput,
    };
    use zksync_os_types::{BlockOutput, BlockPubdata, L2_INTEROP_CENTER_ADDRESS};

    fn block_with_bundle(success: bool) -> BlockOutput {
        let message = vec![0x01, 0x12, 0x34];
        let tx_output = TxOutput {
            execution_result: if success {
                ExecutionResult::Success(ExecutionOutput::Call(Vec::new()))
            } else {
                ExecutionResult::Revert(Vec::new())
            },
            gas_used: 0,
            gas_refunded: 0,
            computational_native_used: 0,
            native_used: 0,
            pubdata_used: 0,
            contract_address: None,
            logs: Vec::new(),
            l2_to_l1_logs: vec![L2ToL1LogWithPreimage {
                log: L2ToL1Log {
                    l2_shard_id: 0,
                    is_service: true,
                    tx_number_in_block: 0,
                    sender: L2_TO_L1_MESSENGER_ADDRESS,
                    key: B256::left_padding_from(L2_INTEROP_CENTER_ADDRESS.as_slice()),
                    value: keccak256(&message),
                },
                preimage: Some(message),
            }],
            storage_writes: Vec::new(),
        };
        BlockOutput {
            header: Header::default().seal_slow(),
            tx_results: vec![Ok(tx_output)],
            storage_writes: Vec::new(),
            account_diffs: Vec::new(),
            published_preimages: Vec::new(),
            pubdata: BlockPubdata::new(0),
            computational_native_used: 0,
        }
    }

    #[test]
    fn interop_bundle_signal_requires_the_canonical_messenger_binding() {
        let message = [0x01, 0x12, 0x34];
        let key = B256::left_padding_from(L2_INTEROP_CENTER_ADDRESS.as_slice());
        let value = keccak256(message);

        assert!(is_interop_bundle_log(
            0,
            true,
            L2_TO_L1_MESSENGER_ADDRESS,
            key,
            value,
            &message,
        ));
        assert!(!is_interop_bundle_log(
            0,
            true,
            Address::ZERO,
            key,
            value,
            &message,
        ));
        assert!(!is_interop_bundle_log(
            0,
            true,
            L2_TO_L1_MESSENGER_ADDRESS,
            B256::ZERO,
            value,
            &message,
        ));
        assert!(!is_interop_bundle_log(
            0,
            true,
            L2_TO_L1_MESSENGER_ADDRESS,
            key,
            B256::ZERO,
            &message,
        ));
        assert!(!is_interop_bundle_log(
            0,
            true,
            L2_TO_L1_MESSENGER_ADDRESS,
            key,
            keccak256([0x02, 0x12, 0x34]),
            &[0x02, 0x12, 0x34],
        ));
    }

    #[test]
    fn block_bundle_signal_ignores_reverted_transaction_output() {
        assert!(block_contains_interop_bundle(&block_with_bundle(true)));
        assert!(!block_contains_interop_bundle(&block_with_bundle(false)));

        let mut missing_preimage = block_with_bundle(true);
        missing_preimage.tx_results[0]
            .as_mut()
            .unwrap()
            .l2_to_l1_logs[0]
            .preimage = None;
        assert!(!block_contains_interop_bundle(&missing_preimage));

        let mut spoofed_sender = block_with_bundle(true);
        spoofed_sender.tx_results[0].as_mut().unwrap().l2_to_l1_logs[0]
            .log
            .sender = Address::ZERO;
        assert!(!block_contains_interop_bundle(&spoofed_sender));
    }
}
