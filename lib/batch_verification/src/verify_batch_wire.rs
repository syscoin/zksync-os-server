use crate::main_node::component::BatchVerificationError;
use alloy::sol_types::SolValue;
use anyhow::anyhow;
use zksync_os_batch_types::batcher_model::BatchForSigning;
use zksync_os_contract_interface::IExecutor;
use zksync_os_contract_interface::models::{CommitBatchInfo, StoredBatchInfo};
use zksync_os_network::VerifyBatch;
use zksync_os_types::PubdataMode;

// SYSCOIN: This fresh-only server accepts the sole protocol V32 commit-data ABI.
const CANONICAL_PROTOCOL_MINOR: u16 = 32;

pub(crate) struct VerificationRequest {
    pub batch_number: u64,
    pub first_block_number: u64,
    pub last_block_number: u64,
    pub pubdata_mode: PubdataMode,
    pub request_id: u64,
    pub commit_data: CommitBatchInfo,
    pub prev_commit_data: StoredBatchInfo,
}

impl TryFrom<VerifyBatch> for VerificationRequest {
    type Error = anyhow::Error;

    fn try_from(request: VerifyBatch) -> Result<Self, Self::Error> {
        anyhow::ensure!(
            request.first_block_number <= request.last_block_number,
            "invalid empty batch block range: {}..={}",
            request.first_block_number,
            request.last_block_number,
        );
        let commit_data = decode_commit_data(
            &request.commit_data,
            request.execution_protocol_version,
            request.first_block_number,
            request.last_block_number,
        )?;
        // SYSCOIN: Bind both copies of the batch identity before native replay; the outer fields
        // route the response while the ABI payload is what the verifier would otherwise sign.
        anyhow::ensure!(
            commit_data.batch_number == request.batch_number,
            "commit batch number does not match request"
        );
        let prev_commit_data = IExecutor::StoredBatchInfo::abi_decode(&request.prev_commit_data)
            .map(StoredBatchInfo::from)
            .map_err(|err| anyhow!("Failed to decode previous commit data: {err}"))?;
        anyhow::ensure!(
            prev_commit_data.batch_number.checked_add(1) == Some(request.batch_number),
            "previous batch is not contiguous with request"
        );

        Ok(Self {
            batch_number: request.batch_number,
            first_block_number: request.first_block_number,
            last_block_number: request.last_block_number,
            pubdata_mode: PubdataMode::from_u8(request.pubdata_mode)
                .ok_or_else(|| anyhow!("Unsupported pubdata mode: {}", request.pubdata_mode))?,
            request_id: request.request_id,
            commit_data,
            prev_commit_data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::dummy_batch_envelope;
    use alloy::primitives::Bytes;

    #[test]
    fn verification_request_rejects_inverted_block_range_before_decoding() {
        let result = VerificationRequest::try_from(VerifyBatch {
            request_id: 1,
            batch_number: 1,
            first_block_number: 2,
            last_block_number: 1,
            pubdata_mode: PubdataMode::Blobs.to_u8(),
            commit_data: Bytes::new(),
            prev_commit_data: Bytes::new(),
            execution_protocol_version: CANONICAL_PROTOCOL_MINOR,
        });
        let Err(err) = result else {
            panic!("inverted batch range was accepted");
        };
        assert!(err.to_string().contains("invalid empty batch block range"));
    }

    #[test]
    fn verification_request_binds_outer_and_commit_batch_numbers() {
        let mut request = encode_verify_batch_request(&dummy_batch_envelope(7, 1, 1), 1).unwrap();
        request.batch_number = 8;

        let Err(err) = VerificationRequest::try_from(request) else {
            panic!("mismatched outer and commit batch numbers were accepted");
        };
        assert!(
            err.to_string()
                .contains("commit batch number does not match request")
        );
    }

    #[test]
    fn verification_request_requires_a_contiguous_predecessor() {
        let mut request = encode_verify_batch_request(&dummy_batch_envelope(7, 1, 1), 1).unwrap();
        let mut previous =
            IExecutor::StoredBatchInfo::abi_decode(&request.prev_commit_data).unwrap();
        previous.batchNumber = u64::MAX;
        request.prev_commit_data = previous.abi_encode().into();

        let Err(err) = VerificationRequest::try_from(request) else {
            panic!("non-contiguous previous batch was accepted");
        };
        assert!(
            err.to_string()
                .contains("previous batch is not contiguous with request")
        );
    }
}

pub(crate) fn encode_verify_batch_request<E>(
    batch_envelope: &BatchForSigning<E>,
    request_id: u64,
) -> Result<VerifyBatch, BatchVerificationError> {
    let execution_protocol_version =
        u16::try_from(batch_envelope.batch.batch_info.protocol_version.minor)
            .map_err(|_| BatchVerificationError::Internal("protocol version overflow".into()))?;
    let commit_data = encode_commit_data(
        batch_envelope.batch.batch_info.commit_info.clone(),
        execution_protocol_version,
    )?;
    let prev_commit_data =
        IExecutor::StoredBatchInfo::from(&batch_envelope.batch.previous_stored_batch_info)
            .abi_encode();

    Ok(VerifyBatch {
        request_id,
        batch_number: batch_envelope.batch_number(),
        first_block_number: batch_envelope.batch.first_block_number,
        last_block_number: batch_envelope.batch.last_block_number,
        pubdata_mode: batch_envelope.batch.pubdata_mode.to_u8(),
        commit_data: commit_data.into(),
        prev_commit_data: prev_commit_data.into(),
        execution_protocol_version,
    })
}

fn decode_commit_data(
    commit_data: &[u8],
    execution_protocol_version: u16,
    first_block_number: u64,
    last_block_number: u64,
) -> anyhow::Result<CommitBatchInfo> {
    anyhow::ensure!(
        execution_protocol_version == CANONICAL_PROTOCOL_MINOR,
        "unsupported execution protocol version: {execution_protocol_version}"
    );
    let decoded: CommitBatchInfo = IExecutor::CommitBatchInfoZKsyncOS::abi_decode(commit_data)
        .map(Into::into)
        .map_err(|err| anyhow!("failed to decode canonical commit data: {err}"))?;
    anyhow::ensure!(
        decoded.first_block_number == Some(first_block_number)
            && decoded.last_block_number == Some(last_block_number),
        "commit block range does not match request"
    );
    Ok(decoded)
}

fn encode_commit_data(
    commit_info: CommitBatchInfo,
    protocol_version_minor: u16,
) -> Result<Vec<u8>, BatchVerificationError> {
    if protocol_version_minor != CANONICAL_PROTOCOL_MINOR {
        return Err(BatchVerificationError::Internal(format!(
            "unsupported protocol version: {protocol_version_minor}"
        )));
    }
    Ok(IExecutor::CommitBatchInfoZKsyncOS::from(commit_info).abi_encode())
}
