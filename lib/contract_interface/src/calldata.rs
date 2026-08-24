use crate::models::{CommitBatchInfo, StoredBatchInfo};
use crate::{IExecutor, IMultisigCommitter};
use alloy::primitives::Address;
use alloy::sol_types::{SolCall, SolValue};

const COMMIT_ENCODING_VERSION: u8 = 4;
// SYSCOIN: This fresh-only contract interface emits the sole protocol V32 commit ABI.
const CANONICAL_PROTOCOL_MINOR: u64 = 32;

pub struct CommitCalldata {
    pub chain_address: Address,
    pub process_from: u64,
    pub process_to: u64,
    pub stored_batch_info: StoredBatchInfo,
    pub commit_batch_info: CommitBatchInfo,
}

impl CommitCalldata {
    pub fn decode(data: &[u8]) -> anyhow::Result<Self> {
        // Check if data is long enough to contain a selector
        if data.len() < 4 {
            anyhow::bail!("data too short to contain function selector");
        }

        // Extract the 4-byte function selector
        let selector = &data[0..4];

        let (chain_address, process_from, process_to, commit_data) =
            if selector == IExecutor::commitBatchesSharedBridgeCall::SELECTOR {
                let commit_call =
                    <IExecutor::commitBatchesSharedBridgeCall as SolCall>::abi_decode(data)?;
                (
                    commit_call._chainAddress,
                    commit_call._processFrom.to(),
                    commit_call._processTo.to(),
                    commit_call._commitData,
                )
            } else if selector == IMultisigCommitter::commitBatchesMultisigCall::SELECTOR {
                let commit_call =
                    <IMultisigCommitter::commitBatchesMultisigCall as SolCall>::abi_decode(data)?;
                (
                    commit_call.chainAddress,
                    commit_call._processBatchFrom.to(),
                    commit_call._processBatchTo.to(),
                    commit_call._batchData,
                )
            } else {
                anyhow::bail!(
                    "unknown function selector: 0x{}",
                    alloy::hex::encode(selector)
                );
            };

        // SYSCOIN: malformed multisig commit calldata can contain an empty batch payload.
        // Reject it before reading the encoding byte so RPC admission returns an error
        // instead of panicking.
        let Some(&encoding_version) = commit_data.first() else {
            anyhow::bail!("commit data is empty");
        };

        if encoding_version != COMMIT_ENCODING_VERSION {
            anyhow::bail!("unexpected encoding version: {}", encoding_version);
        }

        let (stored_batch_info, mut commit_batch_infos) =
            <(
                IExecutor::StoredBatchInfo,
                Vec<IExecutor::CommitBatchInfoZKsyncOS>,
            )>::abi_decode_params(&commit_data[1..])?;
        if commit_batch_infos.len() != 1 {
            anyhow::bail!(
                "unexpected number of committed batch infos: {}",
                commit_batch_infos.len()
            );
        }
        let stored_batch_info = StoredBatchInfo::from(stored_batch_info);
        let commit_batch_info = CommitBatchInfo::from(commit_batch_infos.remove(0));
        Ok(Self {
            chain_address,
            process_from,
            process_to,
            stored_batch_info,
            commit_batch_info,
        })
    }
}

/// This function encodes only the last argument for commitBatchesSharedBridgeCall!
/// Implemented outside of struct to allow only passing necessary arguments
pub fn encode_commit_batch_data(
    prev_batch_info: &StoredBatchInfo,
    commit_info: CommitBatchInfo,
    protocol_version_minor: u64,
) -> Vec<u8> {
    let stored_batch_info = IExecutor::StoredBatchInfo::from(prev_batch_info);
    assert_eq!(
        protocol_version_minor, CANONICAL_PROTOCOL_MINOR,
        "unsupported protocol version"
    );
    let commit_batch_info = IExecutor::CommitBatchInfoZKsyncOS::from(commit_info);
    tracing::debug!(
        last_batch_hash = ?prev_batch_info.hash(),
        last_batch_number = ?prev_batch_info.batch_number,
        new_batch_number = ?commit_batch_info.batchNumber,
        "preparing commit calldata"
    );
    let encoded_data = (stored_batch_info, vec![commit_batch_info]).abi_encode_params();
    [[COMMIT_ENCODING_VERSION].to_vec(), encoded_data].concat()
}
